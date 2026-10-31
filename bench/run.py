#!/usr/bin/env python3
"""云农之窗 · 套餐通道 SSE 冒烟基准（bench/run.py）。

只做一件事：把 `bench/cases/*.jsonl` 里的问句逐条打到 `POST /api/v1/chat/stream`，
量 TTFT、总耗时、token 与错误，写成 JSON；再跟 `bench/baseline.json` 比一遍，
用来判断一次重构/升级有没有把"慢、贵、错"带进来。

只依赖 Python 3.8+ 标准库，不连数据库、不读仓库配置、不写仓库文件（--out 除外）。
不会伪造数据：上游没给 usage 时 token 记 null。

退出码：
  0  跑完，且相对 baseline 没有退化（baseline 不存在时也返回 0）
  1  相对 baseline 退化：p95 TTFT 或 mean 总耗时 > +10%，或 error_rate 增加 > 5pp
  2  参数/用例文件/baseline 文件不合法，或服务不可达（不会打印 Python traceback）
"""

import argparse
import json
import statistics
import sys
import time
from pathlib import Path
from urllib import error as urlerror
from urllib import request as urlrequest

SCRIPT_DIR = Path(__file__).resolve().parent
DEFAULT_CASES = SCRIPT_DIR / "cases" / "smoke.jsonl"
DEFAULT_BASELINE = SCRIPT_DIR / "baseline.json"

# 成本只用来看趋势，不是账单：套餐按时间授权，服务端只对 1% 请求做 token 抽样。
# 价目（元 / 百万 token）按 deepseek-chat 量级取的示意值，换模型时改这里即可。
PRICE_IN_YUAN_PER_MTOK = 2.0
PRICE_OUT_YUAN_PER_MTOK = 8.0

# 本地基准走直连：显式禁用 HTTP(S)_PROXY，避免公司代理缓冲把 SSE 的 TTFT 计时搞脏。
# 如果要打的是必须走代理的外网地址，把下面这行换成默认 opener 即可。
_OPENER = urlrequest.build_opener(urlrequest.ProxyHandler({}))

CHAT_PATH = "/api/v1/chat/stream"


class BenchError(Exception):
    """配置/输入错误 → 退出码 2。"""


class ServiceUnavailable(Exception):
    """服务连不上 → 退出码 2。"""


def _force_utf8_stdout():
    """Windows 上输出被重定向时默认用本地编码（cp936），中文帮助/路径会乱码。"""
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError, OSError):
            pass


def _now_ms(t0):
    return round((time.perf_counter() - t0) * 1000.0, 1)


def _is_timeout(reason):
    if isinstance(reason, TimeoutError):
        return True
    return "timed out" in str(reason).lower()


def _load_json(text):
    try:
        return json.loads(text)
    except (TypeError, ValueError):
        return None


def _as_int(value):
    if isinstance(value, bool) or value is None:
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return int(value)
    if isinstance(value, str) and value.strip().isdigit():
        return int(value.strip())
    return None


def _error_code_from_obj(obj):
    """从统一错误信封（或裸 {code}）里取 code。"""
    if not isinstance(obj, dict):
        return None
    code = obj.get("code")
    if isinstance(code, str) and code:
        return code
    err = obj.get("error")
    if isinstance(err, dict):
        code = err.get("code")
        if isinstance(code, str) and code:
            return code
    return None


def _error_code_from_body(raw):
    if not raw:
        return None
    try:
        obj = json.loads(raw.decode("utf-8", "replace"))
    except ValueError:
        return None
    return _error_code_from_obj(obj)


def _delta_text(data):
    obj = _load_json(data)
    if isinstance(obj, dict):
        for key in ("text", "content"):
            value = obj.get(key)
            if isinstance(value, str):
                return value
        return ""
    return data


def iter_sse(stream):
    """把 SSE 字节流切成 (event, data) 事件，按空行分帧，忽略注释/心跳。"""
    event = None
    data_lines = []
    for raw in stream:
        if isinstance(raw, bytes):
            line = raw.decode("utf-8", "replace")
        else:
            line = raw
        line = line.rstrip("\r\n")
        if not line:
            if event is not None or data_lines:
                yield event or "message", "\n".join(data_lines)
            event = None
            data_lines = []
            continue
        if line.startswith(":"):
            continue
        field, _, value = line.partition(":")
        if value.startswith(" "):
            value = value[1:]
        if field == "event":
            event = value.strip()
        elif field == "data":
            data_lines.append(value)
    if event is not None or data_lines:
        yield event or "message", "\n".join(data_lines)


def load_cases(path):
    """读 JSONL，逐行校验；空行跳过。"""
    if not path.is_file():
        raise BenchError("cases file not found: %s" % path)
    cases = []
    seen = set()
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise BenchError("cannot read cases file %s: %s" % (path, exc))
    for lineno, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line:
            continue
        try:
            case = json.loads(line)
        except ValueError as exc:
            raise BenchError("%s:%d: invalid JSON (%s)" % (path, lineno, exc))
        if not isinstance(case, dict):
            raise BenchError("%s:%d: case must be a JSON object" % (path, lineno))
        case_id = case.get("id")
        question = case.get("question")
        if not isinstance(case_id, str) or not case_id.strip():
            raise BenchError("%s:%d: missing string field 'id'" % (path, lineno))
        if case_id in seen:
            raise BenchError("%s:%d: duplicate id %r" % (path, lineno, case_id))
        if not isinstance(question, str) or not question.strip():
            raise BenchError("%s:%d: missing string field 'question'" % (path, lineno))
        expect = case.get("expect") or {}
        if not isinstance(expect, dict):
            raise BenchError("%s:%d: 'expect' must be an object" % (path, lineno))
        contains = expect.get("contains") or []
        if isinstance(contains, str):
            contains = [contains]
        if not isinstance(contains, list) or any(not isinstance(t, str) for t in contains):
            raise BenchError("%s:%d: 'expect.contains' must be a string list" % (path, lineno))
        seen.add(case_id)
        cases.append(case)
    if not cases:
        raise BenchError("no cases in %s" % path)
    return cases


def load_baseline(path):
    """baseline 不存在返回 None（按约定不算失败）；存在但坏了就是配置错误。"""
    if not path.exists():
        return None
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise BenchError("cannot read baseline %s: %s" % (path, exc))
    if not isinstance(data, dict) or not isinstance(data.get("summary"), dict):
        raise BenchError("baseline %s has no 'summary' object" % path)
    return data


def build_payload(case_id, question):
    return {
        "session_id": "bench",
        "turn_id": "bench-%s" % case_id,
        "round": 1,
        "model": "deepseek-chat",
        "protocol": "openai_completions",
        "messages": [{"role": "user", "content": question}],
        "tools": [],
        "max_output_tokens": 512,
        "stream": True,
        "estimated_input_tokens": 16,
    }


def preflight(base_url, timeout):
    """先探一次 /healthz：完全连不上就退 2，避免每条用例都干等 timeout。

    有 HTTP 响应就算"服务在"——404/500 交给后续用例暴露，不在这里武断退出。
    """
    probe_timeout = min(max(timeout, 1.0), 5.0)
    req = urlrequest.Request(base_url + "/healthz", headers={"Accept": "application/json"})
    try:
        with _OPENER.open(req, timeout=probe_timeout) as resp:
            resp.read(1024)
    except urlerror.HTTPError:
        return
    except (urlerror.URLError, TimeoutError, OSError) as exc:
        raise ServiceUnavailable("cannot reach %s/healthz: %s" % (base_url, exc))


def run_case(base_url, case, timeout, token=None):
    """跑一条用例；返回 (result, 拼起来的回答文本)。连接级失败抛 ServiceUnavailable。"""
    case_id = case["id"]
    payload = build_payload(case_id, case["question"])
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    headers = {
        "Content-Type": "application/json; charset=utf-8",
        "Accept": "text/event-stream",
    }
    if token:
        headers["Authorization"] = "Bearer %s" % token
    req = urlrequest.Request(base_url + CHAT_PATH, data=body, headers=headers, method="POST")

    result = {
        "id": case_id,
        "ok": False,
        "ttft_ms": None,
        "total_ms": None,
        "tokens_in": None,
        "tokens_out": None,
        "error_code": None,
    }
    t0 = time.perf_counter()
    first_delta_at = None
    saw_done = False
    saw_error_code = None
    delta_parts = []

    try:
        resp = _OPENER.open(req, timeout=timeout)
    except urlerror.HTTPError as exc:
        raw = b""
        try:
            raw = exc.read()
        except OSError:
            pass
        result["total_ms"] = _now_ms(t0)
        result["error_code"] = _error_code_from_body(raw) or ("HTTP_%d" % exc.code)
        return result, ""
    except urlerror.URLError as exc:
        if _is_timeout(exc.reason):
            # 服务在，但迟迟不给响应头：算这一条超时，不中止整轮基准。
            result["total_ms"] = _now_ms(t0)
            result["error_code"] = "TIMEOUT"
            return result, ""
        raise ServiceUnavailable("cannot reach %s: %s" % (base_url, exc))
    except TimeoutError:
        result["total_ms"] = _now_ms(t0)
        result["error_code"] = "TIMEOUT"
        return result, ""
    except OSError as exc:
        if _is_timeout(exc):
            result["total_ms"] = _now_ms(t0)
            result["error_code"] = "TIMEOUT"
            return result, ""
        raise ServiceUnavailable("cannot reach %s: %s" % (base_url, exc))

    try:
        with resp:
            content_type = (resp.headers.get("Content-Type") or "").lower()
            if "application/json" in content_type:
                # 有些实现会把错误信封以 200 + JSON 返回，这里按错误处理而不是当空流。
                raw = resp.read()
                result["error_code"] = _error_code_from_body(raw) or "UNEXPECTED_JSON"
            else:
                for event, data in iter_sse(resp):
                    if event == "delta":
                        if first_delta_at is None:
                            first_delta_at = time.perf_counter()
                        delta_parts.append(_delta_text(data))
                    elif event == "usage":
                        obj = _load_json(data)
                        if isinstance(obj, dict):
                            result["tokens_in"] = _as_int(obj.get("tokens_in", obj.get("input_tokens")))
                            result["tokens_out"] = _as_int(obj.get("tokens_out", obj.get("output_tokens")))
                    elif event == "error":
                        saw_error_code = _error_code_from_obj(_load_json(data)) or "UPSTREAM_ERROR"
                    elif event == "done":
                        saw_done = True
                        break
                    # meta / tool_call / ping 不参与计时与断言
    except TimeoutError:
        result["error_code"] = "TIMEOUT"
    except OSError as exc:
        result["error_code"] = "TIMEOUT" if _is_timeout(exc) else "TRANSPORT_ERROR"

    result["total_ms"] = _now_ms(t0)
    if first_delta_at is not None:
        result["ttft_ms"] = round((first_delta_at - t0) * 1000.0, 1)
    if saw_error_code:
        result["error_code"] = saw_error_code
    elif not saw_done and result["error_code"] is None:
        result["error_code"] = "STREAM_INCOMPLETE"
    result["ok"] = saw_done and result["error_code"] is None
    return result, "".join(delta_parts)


def percentile(values, pct):
    """线性插值分位数（样本少时不至于被最近的极值吃掉）。"""
    ordered = sorted(values)
    if not ordered:
        return None
    if len(ordered) == 1:
        return float(ordered[0])
    pos = (len(ordered) - 1) * (pct / 100.0)
    low = int(pos)
    high = min(low + 1, len(ordered) - 1)
    frac = pos - low
    return ordered[low] + (ordered[high] - ordered[low]) * frac


def case_cost_cents(tokens_in, tokens_out):
    ti = tokens_in if isinstance(tokens_in, (int, float)) else 0
    to = tokens_out if isinstance(tokens_out, (int, float)) else 0
    return (ti / 1_000_000.0 * PRICE_IN_YUAN_PER_MTOK + to / 1_000_000.0 * PRICE_OUT_YUAN_PER_MTOK) * 100.0


def summarize(results):
    ttfts = [r["ttft_ms"] for r in results if r["ttft_ms"] is not None]
    totals = [r["total_ms"] for r in results if r["total_ms"] is not None]
    costs = [case_cost_cents(r["tokens_in"], r["tokens_out"]) for r in results]
    total = float(len(results))
    return {
        "p50_ttft_ms": round(percentile(ttfts, 50), 1) if ttfts else None,
        "p95_ttft_ms": round(percentile(ttfts, 95), 1) if ttfts else None,
        "mean_total_ms": round(statistics.mean(totals), 1) if totals else None,
        "error_rate": round(1.0 - sum(1 for r in results if r["ok"]) / total, 4) if total else None,
        "est_cost_per_case_cents": round(statistics.mean(costs), 4) if costs else None,
    }


def _num(value):
    if isinstance(value, bool) or value is None:
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def compare_with_baseline(summary, baseline):
    """返回退化原因列表；空列表表示通过。只比 p95 TTFT / mean 总耗时 / error_rate。"""
    base = baseline.get("summary") or {}
    reasons = []

    base_p95 = _num(base.get("p95_ttft_ms"))
    if base_p95 and summary.get("p95_ttft_ms") is not None:
        if summary["p95_ttft_ms"] > base_p95 * 1.10:
            delta = summary["p95_ttft_ms"] / base_p95 - 1.0
            reasons.append(
                "p95_ttft_ms %.1f > baseline %.1f (+%.1f%% > +10%%)"
                % (summary["p95_ttft_ms"], base_p95, delta * 100.0)
            )

    base_total = _num(base.get("mean_total_ms"))
    if base_total and summary.get("mean_total_ms") is not None:
        if summary["mean_total_ms"] > base_total * 1.10:
            delta = summary["mean_total_ms"] / base_total - 1.0
            reasons.append(
                "mean_total_ms %.1f > baseline %.1f (+%.1f%% > +10%%)"
                % (summary["mean_total_ms"], base_total, delta * 100.0)
            )

    base_err = _num(base.get("error_rate"))
    if base_err is not None and summary.get("error_rate") is not None:
        if summary["error_rate"] > base_err + 0.05 + 1e-9:
            reasons.append(
                "error_rate %.4f > baseline %.4f (+%.2fpp > +5pp)"
                % (summary["error_rate"], base_err, (summary["error_rate"] - base_err) * 100.0)
            )
    return reasons


def write_result(out_path, results, summary):
    out_path.parent.mkdir(parents=True, exist_ok=True)
    doc = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "cases": results,
        "summary": summary,
    }
    out_path.write_text(json.dumps(doc, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return out_path


def _fmt_expect(case, answer):
    expect = case.get("expect") or {}
    terms = expect.get("contains") or []
    if isinstance(terms, str):
        terms = [terms]
    missing = [t for t in terms if t and t not in answer]
    if not terms:
        return "expect: -"
    if missing:
        return "expect: MISS(%s)" % ",".join(missing)
    return "expect: ok"


def _print_unreachable(exc):
    print("ERROR: %s" % exc, file=sys.stderr)
    print(
        "hint: start the server first, e.g. `cargo run -p yunong-server` "
        "or `docker compose -f deploy/docker-compose.yml up -d`",
        file=sys.stderr,
    )


def main(argv=None):
    _force_utf8_stdout()
    parser = argparse.ArgumentParser(
        description="云农之窗 chat/stream 冒烟基准：TTFT / 总耗时 / token / 错误率，并与 baseline 比退化。",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "退出码: 0 无退化或无 baseline | 1 相对 baseline 退化 | 2 参数/文件错误或服务不可达\n"
            "示例: python bench/run.py --cases bench/cases/smoke.jsonl --out bench/out/result.json"
        ),
    )
    parser.add_argument("--base-url", default="http://127.0.0.1:8080", help="服务地址 origin（默认 %(default)s）")
    parser.add_argument("--cases", default=str(DEFAULT_CASES), help="JSONL 用例文件（默认 bench/cases/smoke.jsonl）")
    parser.add_argument("--baseline", default=str(DEFAULT_BASELINE), help="基线 JSON（默认 bench/baseline.json）")
    parser.add_argument("--out", default=None, help="结果 JSON 路径（默认 bench/out/bench-<UTC 时间>.json）")
    parser.add_argument("--timeout", type=float, default=60.0, help="单条用例超时秒数（默认 %(default)s）")
    parser.add_argument("--token", default=None, help="可选 Bearer token；本地免鉴权服务不用传")
    args = parser.parse_args(argv)

    base_url = args.base_url.strip().rstrip("/")
    if not (base_url.startswith("http://") or base_url.startswith("https://")):
        print("ERROR: --base-url must start with http:// or https://", file=sys.stderr)
        return 2
    if args.timeout <= 0:
        print("ERROR: --timeout must be > 0", file=sys.stderr)
        return 2

    try:
        cases = load_cases(Path(args.cases))
        baseline_path = Path(args.baseline)
        baseline = load_baseline(baseline_path)
    except BenchError as exc:
        print("ERROR: %s" % exc, file=sys.stderr)
        return 2

    if args.out:
        out_path = Path(args.out)
    else:
        stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        out_path = SCRIPT_DIR / "out" / ("bench-%s.json" % stamp)

    print("bench: %d cases -> %s%s" % (len(cases), base_url, CHAT_PATH))
    try:
        preflight(base_url, args.timeout)
    except ServiceUnavailable as exc:
        _print_unreachable(exc)
        return 2

    results = []
    for index, case in enumerate(cases, 1):
        try:
            result, answer = run_case(base_url, case, args.timeout, args.token)
        except ServiceUnavailable as exc:
            _print_unreachable(exc)
            return 2
        results.append(result)
        status = "ok" if result["ok"] else "FAIL(%s)" % (result["error_code"] or "UNKNOWN")
        ttft = "n/a" if result["ttft_ms"] is None else "%.1fms" % result["ttft_ms"]
        total = "n/a" if result["total_ms"] is None else "%.1fms" % result["total_ms"]
        tokens = "%s/%s" % (
            "-" if result["tokens_in"] is None else result["tokens_in"],
            "-" if result["tokens_out"] is None else result["tokens_out"],
        )
        print(
            "[%2d/%2d] %-6s ttft=%-9s total=%-9s tokens=%-9s %-28s %s"
            % (index, len(cases), case["id"], ttft, total, tokens, status, _fmt_expect(case, answer))
        )

    summary = summarize(results)
    try:
        write_result(out_path, results, summary)
    except OSError as exc:
        print("ERROR: cannot write result file %s: %s" % (out_path, exc), file=sys.stderr)
        return 2

    print("")
    print("result: %s" % out_path)
    print(
        "summary: p50_ttft=%s p95_ttft=%s mean_total=%s error_rate=%s est_cost_per_case=%s cents"
        % (
            _fmt_ms(summary["p50_ttft_ms"]),
            _fmt_ms(summary["p95_ttft_ms"]),
            _fmt_ms(summary["mean_total_ms"]),
            "-" if summary["error_rate"] is None else "%.4f" % summary["error_rate"],
            "-" if summary["est_cost_per_case_cents"] is None else "%.4f" % summary["est_cost_per_case_cents"],
        )
    )

    if baseline is None:
        print("baseline: %s not found -> gate skipped (exit 0)" % baseline_path)
        return 0

    base = baseline.get("summary") or {}
    print(
        "baseline: p50_ttft=%s p95_ttft=%s mean_total=%s error_rate=%s"
        % (
            base.get("p50_ttft_ms"),
            base.get("p95_ttft_ms"),
            base.get("mean_total_ms"),
            base.get("error_rate"),
        )
    )
    reasons = compare_with_baseline(summary, baseline)
    if reasons:
        for reason in reasons:
            print("REGRESSION: %s" % reason, file=sys.stderr)
        print("FAIL: %d regression(s) against baseline" % len(reasons), file=sys.stderr)
        return 1
    print("PASS: no regression against baseline")
    return 0


def _fmt_ms(value):
    return "-" if value is None else "%.1fms" % value


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        sys.exit(130)
