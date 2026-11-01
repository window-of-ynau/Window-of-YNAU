#!/usr/bin/env python3
"""假上游（mock upstream）：给端到端闭环测试用，不校验 Key，任何字符串都接受。

按请求体里的 ``model`` 字段决定返回形态（一次启动就能覆盖所有场景）：

| model              | 行为                                                                |
|--------------------|---------------------------------------------------------------------|
| ``mock-ok``（默认） | 200 ``text/event-stream``：role → 两段 delta → 带 usage 的结束块 → ``[DONE]`` |
| ``mock-500``       | 500 JSON，正文里带 ``UPSTREAM_SECRET_MARKER_E2E``（用来断言"不泄露上游原文"） |
| ``mock-html``      | 200 ``text/html``（模拟被 SSO/代理劫持），正文带同一个 marker           |
| ``mock-hang``      | 连响应头都不发（用来验证"上游无响应 → 504"）                           |
| ``mock-silent``    | 发响应头但一个事件都不给（验证流内"首事件超时 → event: error"）          |
| ``mock-stream-error`` | 先吐一段 delta，再发厂商错误对象（用来验证流内错误 → ``event: error`` 且洗掉原文） |

用法::

    python3 scripts/mock-upstream/mock_upstream.py --host 0.0.0.0 --port 18765
    # 然后让服务端把 UPSTREAM_*_URL 指向 http://host.docker.internal:18765
    #
    # 注意 `--host 0.0.0.0`：Linux runner 上容器里的 `host.docker.internal` 指向 docker
    # 网关 IP（不是宿主机回环），只监听 127.0.0.1 会连不上。Docker Desktop 会做转发，
    # 两种写法本机都能通，所以统一用 0.0.0.0（mock 只活几秒、不接受真实凭据）。

说明：这是 infra 的 CI 闭环专用 mock。仓库里另有 ``crates/core/examples/fake_sse.rs``
（lead-client 为人工联调加的），它只支持 ok/429/html 三种形态、且不含流内错误帧与挂起，
覆盖不了 ``event: error`` 与超时路径，所以这里单独实现（不改 ``crates/**``）。
"""

import argparse
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# 故意放进上游正文的哨兵串：服务端对外响应里绝不能出现它。
MARKER = "UPSTREAM_SECRET_MARKER_E2E"

# 与 crates/server/tests/chat_stream.rs 的 completions_frames() 同款 wire 格式
FRAMES_OK = [
    '{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}',
    '{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"你好"}}]}',
    '{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"，同学"}}]}',
    '{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],'
    '"usage":{"prompt_tokens":11,"completion_tokens":7}}',
    "[DONE]",
]

FRAMES_STREAM_ERROR = [
    '{"id":"c1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"先吐一点"}}]}',
    '{"error":{"message":"%s daily-quota-exceeded org-abc123 http://10.9.9.9/internal",'
    '"type":"insufficient_quota","code":"insufficient_quota"}}' % MARKER,
    "[DONE]",
]

HANG_SECONDS = 30


class Handler(BaseHTTPRequestHandler):
    # HTTP/1.0：不设 Content-Length，用连接关闭界定 body 结束，天然支持"流式边写边发"
    protocol_version = "HTTP/1.0"

    def log_message(self, fmt, *args):  # noqa: A003 - 覆盖父类，避免刷屏
        pass

    def _write(self, status, content_type, body, extra_headers=None):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        for key, value in (extra_headers or {}).items():
            self.send_header(key, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        self.wfile.flush()

    def do_POST(self):  # noqa: N802 - BaseHTTPRequestHandler 的约定
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b"{}"
        try:
            model = json.loads(raw).get("model") or ""
        except (ValueError, AttributeError):
            model = ""
        print(f"mock-upstream: POST {self.path} model={model}", flush=True)

        if model == "mock-500":
            body = json.dumps(
                {
                    "error": {
                        "message": f"{MARKER} internal-server-error org-abc123",
                        "type": "server_error",
                    }
                }
            ).encode()
            self._write(500, "application/json", body)
            return

        if model == "mock-html":
            body = (
                "<html><head><title>统一身份认证</title></head>"
                f"<body>{MARKER} 请先登录</body></html>"
            ).encode()
            self._write(200, "text/html; charset=utf-8", body)
            return

        if model == "mock-hang":
            # 连响应头都不发：服务端在 open()（等响应头）阶段就会超时 → 504
            time.sleep(HANG_SECONDS)
            return

        if model == "mock-silent":
            # 响应头照发（所以服务端会先回 200 + meta），但一个事件都不给 →
            # 流内"首事件超时" → `event: error`（UPSTREAM_TIMEOUT）
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.wfile.flush()
            time.sleep(HANG_SECONDS)
            return

        # 正常流 / 流内错误帧
        frames = FRAMES_STREAM_ERROR if model == "mock-stream-error" else FRAMES_OK
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        for frame in frames:
            self.wfile.write(f"data: {frame}\n\n".encode())
            self.wfile.flush()
            time.sleep(0.01)


def main() -> int:
    parser = argparse.ArgumentParser(description="假上游（mock upstream）")
    parser.add_argument("--port", type=int, default=18765)
    parser.add_argument("--host", default="127.0.0.1")
    args = parser.parse_args()

    server = ThreadingHTTPServer((args.host, args.port), Handler)
    print(f"mock-upstream: listening on http://{args.host}:{args.port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
