#!/usr/bin/env python3
"""评测用 mock 上游（只依赖标准库）。

为什么需要它：评测门禁要在 CI 里**每条 PR 都跑**，不能用真实厂商 Key（贵、不可复现、还会被
上游抖动/限流干扰）。所以用这个 mock 扮演 `openai_completions` 上游：

- 按 `turn_id`（`bench-<case id>`）从 `bench/cases/full.jsonl` 里取该用例的**标准答案**回放；
- 回放的 SSE 形状与真实上游一致（`chat.completion.chunk` 增量帧 + 末帧 usage），
  这样服务端的 TTFT / token / usage 链路都是**真跑**的，只有"模型内容"是假的。

诚实边界（写进 bench/README.md）：它验证的是**管线**（流式转发、usage 记账、错误处理、延迟基线），
不是模型质量；要点命中率在这里衡量的是"服务端有没有把上游内容完整、按序送达"。

用法：
    python bench/mock_upstream.py --port 18089 --cases bench/cases/full.jsonl
"""

import argparse
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent


def load_answers(path):
    """问题文本 → 标准答案。

    为什么按**问题**而不是 turn_id 查：`turn_id` 是我们内部字段，服务端转发给上游时
    **不会**把它塞进请求体（上游协议里没有这个字段）。上游能看到的只有 messages。
    缺文件时退化为"回显问题"，不静默假成功。
    """
    answers = {}
    try:
        with open(path, "r", encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                case = json.loads(line)
                question = case.get("question")
                answer = case.get("answer")
                if isinstance(question, str) and isinstance(answer, str):
                    answers[question.strip()] = answer
    except OSError as exc:
        print("warn: 读不到用例文件 %s（%s），回显问题作为答案" % (path, exc), file=sys.stderr)
    return answers


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # 安静点：CI 日志里只留结论
        return

    def _sse_start(self):
        # 用 chunked 分帧：SSE 没有 Content-Length，只有 chunked 或"关连接"两种合法终止方式。
        # 关连接会让服务端的连接池复用到已关闭的 socket，下一次请求被迫重连（实测慢 20–100ms，
        # 且呈现"隔一条慢一次"的假抖动）——所以这里必须 chunked。
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        self._chunked = True

    def _write_chunk(self, data):
        self.wfile.write(b"%x\r\n" % len(data))
        self.wfile.write(data)
        self.wfile.write(b"\r\n")

    def _chunked_end(self):
        # chunked 的终止块：长度为 0 的 chunk + 空行
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()

    def _send_event(self, payload):
        body = "data: %s\n\n" % json.dumps(payload, ensure_ascii=False)
        self._write_chunk(body.encode("utf-8"))
        self.wfile.flush()

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b"{}"
        try:
            request = json.loads(raw.decode("utf-8"))
        except ValueError:
            request = {}

        messages = request.get("messages") or []
        question = ""
        if messages and isinstance(messages[-1], dict):
            question = (messages[-1].get("content") or "").strip()
        answer = self.server.answers.get(question)
        if answer is None:
            # 找不到就诚实地回显问题（不要编一个看着像答案的句子，否则命中率会骗人）
            answer = "关于「%s」：暂时没有更多信息。" % question

        completion_id = "chatcmpl-mock"
        model = request.get("model") or "mock-model"

        self._sse_start()
        # 首帧只带 role（与真实上游一致）：服务端的 TTFT 度量的是"第一个文本增量"
        self._send_event(
            {
                "id": completion_id,
                "object": "chat.completion.chunk",
                "model": model,
                "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": None}],
            }
        )

        # 按固定切片大小回放，切片大小可控（BENCH_CHUNK_CHARS）以便做"分片变大/变小"的退化实验
        chunk_size = self.server.chunk_chars
        for offset in range(0, len(answer), chunk_size):
            piece = answer[offset : offset + chunk_size]
            self._send_event(
                {
                    "id": completion_id,
                    "object": "chat.completion.chunk",
                    "model": model,
                    "choices": [
                        {"index": 0, "delta": {"content": piece}, "finish_reason": None}
                    ],
                }
            )

        # 末帧：同时给 finish_reason 与 usage（服务端的 usage 合并逻辑走真实路径）
        self._send_event(
            {
                "id": completion_id,
                "object": "chat.completion.chunk",
                "model": model,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                "usage": {
                    "prompt_tokens": self.server.tokens_in,
                    "completion_tokens": max(1, len(answer) // 2),
                    "total_tokens": self.server.tokens_in + max(1, len(answer) // 2),
                },
            }
        )
        self._write_chunk(b"data: [DONE]\n\n")
        # 终止块：告诉客户端"body 到此为止"，连接仍可复用（这才是真实上游的行为）
        self._chunked_end()


class MockServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, addr, answers, chunk_chars, tokens_in):
        super().__init__(addr, Handler)
        self.answers = answers
        self.chunk_chars = chunk_chars
        self.tokens_in = tokens_in


def main(argv=None):
    parser = argparse.ArgumentParser(description="评测用 mock 上游（openai_completions SSE）")
    parser.add_argument("--port", type=int, default=18089)
    parser.add_argument("--cases", default=str(SCRIPT_DIR / "cases" / "full.jsonl"))
    parser.add_argument(
        "--chunk-chars",
        type=int,
        default=64,
        help="每个 delta 帧的字符数（越小帧越多；用于做分片相关的退化实验）",
    )
    parser.add_argument(
        "--tokens-in",
        type=int,
        default=400,
        help="回放的 prompt_tokens（固定值，保证 token/成本可复现）",
    )
    parser.add_argument(
        "--delay-ms",
        type=float,
        default=0.0,
        help="每个 delta 帧额外的睡眠毫秒（仅用于人为制造延迟，验证门禁能抓到退化）",
    )
    args = parser.parse_args(argv)

    answers = load_answers(Path(args.cases))
    server = MockServer(("127.0.0.1", args.port), answers, max(1, args.chunk_chars), args.tokens_in)
    if args.delay_ms > 0:
        original = Handler._send_event

        def slow_send(self, payload):
            time.sleep(args.delay_ms / 1000.0)
            original(self, payload)

        Handler._send_event = slow_send

    print(
        "mock upstream: http://127.0.0.1:%d (%d 条标准答案, chunk=%d chars, tokens_in=%d)"
        % (args.port, len(answers), args.chunk_chars, args.tokens_in),
        flush=True,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
