#!/usr/bin/env python3
"""无凭据端到端闭环：mock 上游 ← 服务端容器 ← 脚本客户端。

链路：起 PG（pgvector 镜像）→ 用生产 ``deploy/migrate.sh`` 跑迁移 → 起服务端容器
（``UPSTREAM_*_URL`` 指向本机 mock）→ 注册 → 登录 → ``/api/v1/chat/stream``。

断言（全部走真实 HTTP / 真实容器，不用真实账号或 Key）：

1. **正常流**：HTTP 200、``text/event-stream``；``meta`` 必首、``done`` 必末且唯一、
   含 ``usage``；delta 文本拼接等于 mock 的响应文本。
2. **抽样记账**：``USAGE_SAMPLE_PERCENT=100`` 时 ``usage_samples`` 落 1 行，
   ``tokens_in/out`` 与 mock 给的 usage 一致（11/7），``estimated=false``。
3. **上游 500** → 客户端拿到 HTTP 502 + ``UPSTREAM_ERROR`` 信封，**不泄露上游原文**。
4. **上游非 SSE（200 text/html）** → 502 + 不泄露 HTML/原文。
5. **流内错误帧** → ``event: error`` 是终止事件（其后无 ``done``），文案被洗成公共文案，
   **不泄露厂商原文**（F9 回归）。
6. **首事件超时**（mock 挂起）→ 504 + ``UPSTREAM_TIMEOUT``，不泄露上游原文。
7. **审计**：上面几次上游失败要落 ``audit_logs(decision='error')``（轮询等待，写是异步的）。

用法::

    python3 scripts/e2e-chat.py --image yunong-server:ci
    python3 scripts/e2e-chat.py --image yunong-server:ci --keep   # 失败时留容器排查

依赖：docker、python3（stdlib 即可）。**不需要** psql（迁移与断言都走 pgvector 镜像里的 psql）。
"""

import argparse
import json
import os
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

MARKER = "UPSTREAM_SECRET_MARKER_E2E"
PG_IMAGE = "pgvector/pgvector:pg16"
DB_PASSWORD = "e2e-postgres"
DB_USER = "yunong"
DB_NAME = "yunong"


def log(message: str) -> None:
    print(f"[e2e] {message}", flush=True)


class Failure(RuntimeError):
    pass


def check(condition: bool, message: str) -> None:
    if not condition:
        raise Failure(message)


def run(cmd: list[str], *, capture: bool = True) -> subprocess.CompletedProcess:
    # 固定 UTF-8：Windows 上 subprocess 默认用 GBK 解码子进程输出，docker/psql 的
    # 中文日志会直接抛 UnicodeDecodeError（CI 是 Linux，但本地要能一键复现）。
    return subprocess.run(
        cmd,
        check=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        capture_output=capture,
    )


def docker(*args: str) -> str:
    return run(["docker", *args]).stdout.strip()


def wait_port(host: str, port: int, budget: float, what: str) -> None:
    deadline = time.monotonic() + budget
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=1):
                return
        except OSError:
            time.sleep(0.2)
    raise Failure(f"{what} 在 {budget:.0f}s 内没有就绪（{host}:{port}）")


def wait_http(url: str, budget: float, what: str) -> None:
    deadline = time.monotonic() + budget
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=2) as response:
                if response.status == 200:
                    return
        except OSError:
            pass
        time.sleep(0.3)
    raise Failure(f"{what} 在 {budget:.0f}s 内没有就绪（{url}）")


def http_json(
    method: str,
    url: str,
    body: dict | None = None,
    token: str | None = None,
    timeout: float = 30.0,
) -> tuple[int, dict, bytes, dict]:
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(url, data=data, method=method)
    request.add_header("Content-Type", "application/json")
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            raw = response.read()
            # header 名统一小写：hyper 回的是 "content-type"，Windows 的 http.client 不改大小写
            headers = {name.lower(): value for name, value in response.headers.items()}
            return response.status, headers, raw, json_or_empty(raw)
    except urllib.error.HTTPError as error:
        raw = error.read()
        headers = {name.lower(): value for name, value in error.headers.items()} if error.headers else {}
        return error.code, headers, raw, json_or_empty(raw)


def json_or_empty(raw: bytes) -> dict:
    """SSE / 纯文本响应的 body 不是 JSON，这里兜住，避免调用方还要区分类型。"""
    if not raw:
        return {}
    try:
        loaded = json.loads(raw)
    except ValueError:
        return {}
    return loaded if isinstance(loaded, dict) else {}


def parse_sse(raw: bytes) -> list[tuple[str | None, str]]:
    events: list[tuple[str | None, str]] = []
    for block in raw.decode("utf-8", "replace").split("\n\n"):
        if not block.strip():
            continue
        name: str | None = None
        data: list[str] = []
        for line in block.splitlines():
            if line.startswith("event: "):
                name = line[len("event: ") :]
            elif line.startswith("data: "):
                data.append(line[len("data: ") :])
        events.append((name, "\n".join(data)))
    return events


def chat_body(turn_id: str, model: str) -> dict:
    return {
        "session_id": "e2e-session",
        "turn_id": turn_id,
        "round": 1,
        "model": model,
        "protocol": "openai_completions",
        "messages": [{"role": "user", "content": "用一句话回答：1+1 等于几？"}],
        "tools": [],
        "max_output_tokens": 64,
        "stream": True,
        "estimated_input_tokens": 0,
    }


def psql(db_container: str, sql: str) -> str:
    return docker(
        "exec", db_container,
        "psql", "-U", DB_USER, "-d", DB_NAME, "-tAc", sql,
    )


def wait_for_value(db_container: str, sql: str, what: str, budget: float = 10.0) -> str:
    deadline = time.monotonic() + budget
    last = ""
    while time.monotonic() < deadline:
        last = psql(db_container, sql).strip()
        if last and last != "0":
            return last
        time.sleep(0.3)
    raise Failure(f"等待超时：{what}（最后一次查询结果 = {last!r}）")


def dump_logs(server_container: str) -> None:
    try:
        logs = docker("logs", "--tail", "80", server_container)
        print("---- 服务端容器日志（tail 80）----", flush=True)
        print(logs, flush=True)
    except subprocess.CalledProcessError:
        print("（取服务端容器日志失败，容器可能没起来）", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description="无凭据端到端闭环")
    parser.add_argument("--image", default="yunong-server:ci", help="服务端镜像 tag")
    parser.add_argument("--server-port", type=int, default=18090)
    parser.add_argument("--pg-port", type=int, default=15432)
    parser.add_argument("--mock-port", type=int, default=18765)
    parser.add_argument("--keep", action="store_true", help="失败时保留容器与 mock 便于排查")
    args = parser.parse_args()

    suffix = uuid.uuid4().hex[:8]
    network = f"yunong-e2e-{suffix}-net"
    db_container = f"yunong-e2e-{suffix}-db"
    server_container = f"yunong-e2e-{suffix}-server"
    mock_proc: subprocess.Popen | None = None
    started = False
    base = f"http://127.0.0.1:{args.server_port}"

    def cleanup() -> None:
        if args.keep:
            log("--keep：保留容器与 mock 进程，排查后手动清理")
            return
        for container in (server_container, db_container):
            subprocess.run(
                ["docker", "rm", "-f", container],
                capture_output=True, text=True, encoding="utf-8", errors="replace",
            )
        subprocess.run(
            ["docker", "network", "rm", network],
            capture_output=True, text=True, encoding="utf-8", errors="replace",
        )
        if mock_proc and mock_proc.poll() is None:
            mock_proc.terminate()
            try:
                mock_proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                mock_proc.kill()

    try:
        log("1/7 起 mock 上游（本机进程，容器通过 host.docker.internal 访问）")
        mock_script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "mock-upstream", "mock_upstream.py")
        # 必须绑 0.0.0.0：Linux runner 上容器的 host.docker.internal 是 docker 网关 IP
        # （不是宿主机回环），只监听 127.0.0.1 会连不上（本地 Docker Desktop 会做转发，
        # 所以这个坑只在 CI 上暴露过一次）。mock 只活几秒且不接受真实凭据。
        mock_proc = subprocess.Popen(
            [sys.executable, mock_script, "--host", "0.0.0.0", "--port", str(args.mock_port)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.STDOUT,
        )
        wait_port("127.0.0.1", args.mock_port, 15, "mock 上游")

        log("2/7 起 PostgreSQL（pgvector/pg16）并等就绪")
        docker(
            "run", "-d", "--name", db_container,
            "-e", f"POSTGRES_PASSWORD={DB_PASSWORD}",
            "-e", f"POSTGRES_USER={DB_USER}",
            "-e", f"POSTGRES_DB={DB_NAME}",
            "-p", f"127.0.0.1:{args.pg_port}:5432",
            PG_IMAGE,
        )
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            probe = subprocess.run(
                ["docker", "exec", db_container, "pg_isready", "-U", DB_USER, "-d", DB_NAME],
                capture_output=True, text=True, encoding="utf-8", errors="replace",
            )
            if probe.returncode == 0:
                break
            time.sleep(0.5)
        else:
            raise Failure("PostgreSQL 60s 内没有就绪")

        log("3/7 建 docker 网络 + 用生产 migrate.sh 跑迁移")
        docker("network", "create", network)
        started = True
        docker("network", "connect", network, db_container)
        repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        docker(
            "run", "--rm", "--network", network,
            "-v", f"{repo_root}/crates/server/migrations:/migrations:ro",
            "-v", f"{repo_root}/deploy/migrate.sh:/migrate.sh:ro",
            "-e", f"PGHOST={db_container}",
            "-e", f"PGUSER={DB_USER}",
            "-e", f"PGDATABASE={DB_NAME}",
            "-e", f"PGPASSWORD={DB_PASSWORD}",
            "--entrypoint", "/bin/sh",
            PG_IMAGE, "/migrate.sh",
        )

        log("4/7 起服务端容器（UPSTREAM_*_URL → mock，抽样 100%，首事件超时 2s）")
        docker(
            "run", "-d", "--name", server_container,
            "--network", network,
            "--add-host=host.docker.internal:host-gateway",
            "-p", f"127.0.0.1:{args.server_port}:8080",
            "-e", "YUNONG_BIND=0.0.0.0:8080",
            "-e", f"DATABASE_URL=postgres://{DB_USER}:{DB_PASSWORD}@{db_container}:5432/{DB_NAME}",
            "-e", f"UPSTREAM_COMPLETIONS_URL=http://host.docker.internal:{args.mock_port}",
            "-e", f"UPSTREAM_RESPONSES_URL=http://host.docker.internal:{args.mock_port}",
            "-e", f"UPSTREAM_MESSAGES_URL=http://host.docker.internal:{args.mock_port}",
            "-e", "UPSTREAM_API_KEY=sk-e2e-mock-not-a-real-key",
            "-e", "USAGE_SAMPLE_PERCENT=100",
            "-e", "YUNONG_SAMPLE_SALT=e2e-sample-salt-0123456789",
            "-e", "UPSTREAM_FIRST_EVENT_TIMEOUT_SECS=2",
            "-e", "RUST_LOG=info",
            args.image,
        )
        wait_http(f"{base}/healthz", 60, "服务端 /healthz")
        wait_http(f"{base}/readyz", 30, "服务端 /readyz")

        log("5/7 注册 → 登录")
        email = f"e2e-{suffix}@example.com"
        password = "e2e-Passw0rd!"
        status, _, raw, payload = http_json("POST", f"{base}/api/v1/auth/register", {"email": email, "password": password})
        check(status == 201, f"注册应返回 201，实际 {status}: {raw[:300]!r}")
        # 成功信封：{"ok": true, "data": {...}, "request_id": "req_..."}
        check(bool((payload.get("data") or {}).get("access_token")), f"注册响应里没有 access_token：{payload}")

        status, _, raw, payload = http_json("POST", f"{base}/api/v1/auth/login", {"email": email, "password": password})
        check(status == 200, f"登录应返回 200，实际 {status}: {raw[:300]!r}")
        token = (payload.get("data") or {}).get("access_token")
        check(bool(token), f"登录响应里没有 access_token：{payload}")

        log("6/7 正常流：meta 必首 / usage / done 必末且唯一")
        turn_ok = f"e2e-turn-ok-{suffix}"
        status, headers, raw, _ = http_json(
            "POST", f"{base}/api/v1/chat/stream", chat_body(turn_ok, "mock-ok"), token=token
        )
        check(status == 200, f"正常流应返回 200，实际 {status}: {raw[:300]!r}")
        check("text/event-stream" in headers.get("content-type", ""), f"Content-Type 不是 SSE：{headers.get('content-type')!r}")
        events = parse_sse(raw)
        names = [name for name, _ in events]
        check(bool(events), "正常流没有任何 SSE 事件")
        check(names[0] == "meta", f"首个事件必须是 meta，实际 {names[0]!r}")
        check(names[-1] == "done", f"最后一个事件必须是 done，实际 {names[-1]!r}")
        check(names.count("done") == 1, f"done 必须唯一，实际出现 {names.count('done')} 次")
        check("usage" in names, "缺少 usage 事件")
        check("error" not in names, f"正常流不该有 error 事件：{names}")
        text = "".join(
            json.loads(data).get("text", "")
            for name, data in events
            if name == "delta"
        )
        check(text == "你好，同学", f"delta 文本拼接应等于 mock 文本，实际 {text!r}")

        log("7/7 失败场景 + 记账/审计断言")
        # 7.1 上游 500 → 502 + UPSTREAM_ERROR，不泄露原文
        turn_500 = f"e2e-turn-500-{suffix}"
        status, _, raw, payload = http_json(
            "POST", f"{base}/api/v1/chat/stream", chat_body(turn_500, "mock-500"), token=token
        )
        check(status == 502, f"上游 500 应映射成 502，实际 {status}: {raw[:300]!r}")
        code = (payload.get("error") or {}).get("code")
        check(code == "UPSTREAM_ERROR", f"错误码应为 UPSTREAM_ERROR，实际 {code!r}")
        check(MARKER.encode() not in raw, "响应泄露了上游原文（MARKER 出现在 /chat/stream 响应里）")

        # 7.2 上游非 SSE（200 text/html）→ 502，不泄露 HTML
        turn_html = f"e2e-turn-html-{suffix}"
        status, _, raw, payload = http_json(
            "POST", f"{base}/api/v1/chat/stream", chat_body(turn_html, "mock-html"), token=token
        )
        check(status == 502, f"非 SSE 上游应映射成 502，实际 {status}: {raw[:300]!r}")
        check(MARKER.encode() not in raw, "响应泄露了上游原文（HTML）")
        check(b"<html>" not in raw.lower(), "响应里出现了上游 HTML")

        # 7.3 流内错误帧 → event: error 终止，文案被洗掉
        turn_stream_error = f"e2e-turn-stream-error-{suffix}"
        status, headers, raw, _ = http_json(
            "POST", f"{base}/api/v1/chat/stream", chat_body(turn_stream_error, "mock-stream-error"), token=token
        )
        check(status == 200, f"流内错误应先返回 200，实际 {status}: {raw[:300]!r}")
        events = parse_sse(raw)
        names = [name for name, _ in events]
        check("error" in names, f"流内错误帧应产生 event: error，实际 {names}")
        check(MARKER.encode() not in raw, "流内错误响应泄露了厂商原文（F9 回归）")
        check(names[-1] == "error", f"error 必须是终止事件（其后不能再有事件），实际 {names}")
        error_message = json.loads(dict(events)["error"]).get("message", "")
        check(bool(error_message.strip()), "event: error 的 message 不能为空")

        # 7.4 上游无响应（连响应头都不给）→ 504 + UPSTREAM_TIMEOUT
        turn_hang = f"e2e-turn-hang-{suffix}"
        status, _, raw, payload = http_json(
            "POST", f"{base}/api/v1/chat/stream", chat_body(turn_hang, "mock-hang"), token=token, timeout=20
        )
        check(status == 504, f"上游无响应应映射成 504，实际 {status}: {raw[:300]!r}")
        check((payload.get("error") or {}).get("code") == "UPSTREAM_TIMEOUT", f"错误码应为 UPSTREAM_TIMEOUT，实际 {payload.get('error')}")
        check(MARKER.encode() not in raw, "504 响应泄露了上游原文")

        # 7.5 流内首事件超时（响应头给了、事件不来）→ 200 + event: error
        turn_silent = f"e2e-turn-silent-{suffix}"
        status, headers, raw, _ = http_json(
            "POST", f"{base}/api/v1/chat/stream", chat_body(turn_silent, "mock-silent"), token=token, timeout=20
        )
        check(status == 200, f"流内超时应先返回 200，实际 {status}: {raw[:300]!r}")
        events = parse_sse(raw)
        names = [name for name, _ in events]
        check(names and names[0] == "meta", f"流内超时的首事件应是 meta，实际 {names}")
        check(names[-1] == "error", f"流内超时应以 error 终止，实际 {names}")
        silent_error = json.loads(dict(events)["error"])
        check(silent_error.get("code") == "UPSTREAM_TIMEOUT", f"流内超时的 code 应为 UPSTREAM_TIMEOUT，实际 {silent_error}")

        # 7.6 记账：正常流那一轮必须落 1 行（100% 抽样），token 数与 mock usage 一致
        row = wait_for_value(
            db_container,
            "SELECT count(*) || ':' || coalesce(max(tokens_in), -1) || ':' || coalesce(max(tokens_out), -1) "
            f"|| ':' || coalesce(bool_or(estimated), false) FROM usage_samples WHERE turn_id = '{turn_ok}'",
            "usage_samples 出现正常流的抽样行",
        )
        count, tokens_in, tokens_out, estimated = row.split(":")
        check(count == "1", f"usage_samples 应有 1 行，实际 {count}")
        check(tokens_in == "11" and tokens_out == "7", f"usage 应对齐 mock（11/7），实际 {tokens_in}/{tokens_out}")
        check(estimated in ("f", "false"), f"上游给了 usage，estimated 应为 false，实际 {estimated}")
        log(f"usage_samples: turn={turn_ok} tokens_in={tokens_in} tokens_out={tokens_out} estimated={estimated}")

        # 7.7 审计：三次上游失败（500 / 非 SSE / 超时）至少各写一条 decision=error
        audit = wait_for_value(
            db_container,
            "SELECT count(*) FROM audit_logs WHERE action = 'chat.stream' AND decision = 'error'",
            "audit_logs 写入 decision=error",
        )
        check(int(audit) >= 3, f"audit_logs(decision=error) 至少应有 3 条，实际 {audit}")
        log(f"audit_logs(chat.stream, decision=error) = {audit} 条")

        print(
            "\n".join(
                [
                    "E2E_OK",
                    "  scenarios: ok / upstream-500 / non-sse / stream-error / open-timeout-504 / silent-stream-timeout",
                    f"  usage_samples: turn={turn_ok} tokens_in=11 tokens_out=7 estimated=false",
                    f"  audit_logs(decision=error): {audit}",
                ]
            ),
            flush=True,
        )
        return 0
    except Failure as failure:
        print(f"E2E_FAILED: {failure}", file=sys.stderr, flush=True)
        if started:
            dump_logs(server_container)
        return 1
    except subprocess.CalledProcessError as error:
        print(f"E2E_FAILED(命令失败): {' '.join(error.cmd)}\nstdout={error.stdout}\nstderr={error.stderr}", file=sys.stderr, flush=True)
        if started:
            dump_logs(server_container)
        return 1
    finally:
        cleanup()


if __name__ == "__main__":
    sys.exit(main())

