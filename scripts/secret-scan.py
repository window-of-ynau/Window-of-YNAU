#!/usr/bin/env python3
"""密钥扫描（规则唯一来源）：CI 与 pre-commit 钩子共用同一份规则，别写第二份。

背景：owner 的真实 API Key 曾被写进测试代码并推到 GitHub（已吊销 + 脱敏）。这个脚本
让"把密钥写进代码"在**提交前**（pre-commit）和**合并前**（CI）都直接失败。

用法：
    python3 scripts/secret-scan.py --all        # 扫 git 跟踪的文件（CI 用这个）
    python3 scripts/secret-scan.py --staged     # 只扫暂存区内容（pre-commit 用这个）
    python3 scripts/secret-scan.py --path foo.py   # 扫指定文件/目录（本地调试）
    ALLOW_SECRET=1 python3 scripts/secret-scan.py --all   # 误报放行（会打警告）

退出码：0 = 干净（或 ALLOW_SECRET 放行）；1 = 命中；2 = 用法/环境错误。

规则清单（加规则请同步 CONTRIBUTING.md §4 与 deploy/README.md 的说明）：

| 规则 | 匹配 | 例子 |
|---|---|---|
| `sk-api-key` | `sk-` + ≥20 位字母数字 | DeepSeek / OpenAI 风格 Key；**`sk-test-` 前缀放行** |
| `aws-access-key` | `AKIA` + 16 位大写字母数字 | AWS Access Key ID |
| `github-token` | `gh[pousr]_` + ≥20 位 | GitHub PAT / OAuth / App token |
| `private-key` | `-----BEGIN … PRIVATE KEY-----` | 私钥块 |
| `dsn-with-password` | `postgres://user:password@` 等带密码的 DSN | 数据库连接串 |

`sk-test-` 前缀是**约定**：测试/文档里的示例 Key 一律写成 `sk-test-...`（不会被拦），
真实 Key 不是这个前缀，所以既能放行示例、又拦得住真货。
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys

# 规则清单：命中即算泄露。新增规则时同步 CONTRIBUTING.md §4。
RULES: list[tuple[str, re.Pattern[str]]] = [
    # `sk-test-` 是约定好的示例前缀，放行；真实 Key 不会是 test- 开头。
    ("sk-api-key", re.compile(r"\bsk-(?!test-)[A-Za-z0-9]{20,}")),
    ("aws-access-key", re.compile(r"\bAKIA[0-9A-Z]{16}\b")),
    ("github-token", re.compile(r"\bgh[pousr]_[A-Za-z0-9]{20,}\b")),
    ("private-key", re.compile(r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP )?PRIVATE KEY-----")),
    (
        # 只拦"像真密码"的：>=16 位。本地/CI 的示例口令（postgres / ci-dummy / e2e-postgres）
        # 都短于这个长度；真实 DSN 密码基本是生成的长随机串。阈值写进 CONTRIBUTING.md §4。
        "dsn-with-password",
        re.compile(r"\b(?:postgres(?:ql)?|mysql|redis|mongodb(?:\+srv)?)://[^\s:@/'\"]+:[^\s@/'\"]{16,}@"),
    ),
]

# 占位符：这些串出现在**命中文本**里时视为示例，不算泄露（只对 DSN / 私钥生效）。
PLACEHOLDERS = (
    "CHANGE_ME",
    "changeme",
    "CHANGEME",
    "<password>",
    "<your",
    "***",
    "xxxx",
    "REDACTED",
    "placeholder",
)

# 行内豁免：极少数需要在文档里写"长这样"的规则示例时用。
INLINE_ALLOW = ("secret-scan: allow",)

SKIP_DIRS = (
    ".git/",
    "target/",
    "app/build/",
    ".dart_tool/",
    "node_modules/",
    "__pycache__/",
    "bench/out/",
)

MAX_BYTES = 2 * 1024 * 1024  # 单文件上限：再大基本是数据/产物，没必要逐行扫


def is_binary(chunk: bytes) -> bool:
    return b"\x00" in chunk[:8192]


def mask(matched: str) -> str:
    """只露前 6 个字符 + 长度，避免把密钥打进 CI 日志（日志也是泄露面）。"""
    visible = matched[:6]
    return f"{visible}…（len={len(matched)}）"


def scan_text(path: str, text: str, hits: list[tuple[str, int, str, str]]) -> None:
    for lineno, line in enumerate(text.splitlines(), start=1):
        if any(flag in line for flag in INLINE_ALLOW):
            continue
        for rule, pattern in RULES:
            match = pattern.search(line)
            if not match:
                continue
            matched = match.group(0)
            if rule in ("dsn-with-password", "private-key") and any(p in matched for p in PLACEHOLDERS):
                continue
            hits.append((rule, lineno, path, mask(matched)))


def iter_tracked(repo: str):
    """CI 模式：git 跟踪的文件（不碰 ignored / 未跟踪的本地文件）。"""
    out = subprocess.run(
        ["git", "-C", repo, "ls-files", "-z"],
        check=True,
        capture_output=True,
    ).stdout
    for raw in out.split(b"\0"):
        if not raw:
            continue
        rel = raw.decode("utf-8", "replace")
        if rel.startswith(SKIP_DIRS) or any(f"/{skip}" in f"/{rel}" for skip in SKIP_DIRS):
            continue
        full = os.path.join(repo, rel)
        try:
            if os.path.getsize(full) > MAX_BYTES:
                continue
            with open(full, "rb") as fh:
                data = fh.read()
        except OSError:
            continue
        if is_binary(data):
            continue
        yield rel, data.decode("utf-8", "replace")


def iter_staged(repo: str):
    """pre-commit 模式：暂存区（index）里的内容，而不是工作区。"""
    out = subprocess.run(
        ["git", "-C", repo, "diff", "--cached", "--name-only", "--diff-filter=ACMR", "-z"],
        check=True,
        capture_output=True,
    ).stdout
    for raw in out.split(b"\0"):
        if not raw:
            continue
        rel = raw.decode("utf-8", "replace")
        if rel.startswith(SKIP_DIRS) or any(f"/{skip}" in f"/{rel}" for skip in SKIP_DIRS):
            continue
        blob = subprocess.run(
            ["git", "-C", repo, "show", f":{rel}"],
            check=False,
            capture_output=True,
        )
        if blob.returncode != 0:
            continue
        data = blob.stdout
        if len(data) > MAX_BYTES or is_binary(data):
            continue
        yield rel, data.decode("utf-8", "replace")


def iter_path(path: str):
    if os.path.isdir(path):
        for root, dirs, files in os.walk(path):
            dirs[:] = [d for d in dirs if d not in {".git", "target", "node_modules", "__pycache__", "build"}]
            for name in files:
                full = os.path.join(root, name)
                try:
                    if os.path.getsize(full) > MAX_BYTES:
                        continue
                    with open(full, "rb") as fh:
                        data = fh.read()
                except OSError:
                    continue
                if is_binary(data):
                    continue
                yield full, data.decode("utf-8", "replace")
    else:
        with open(path, "rb") as fh:
            data = fh.read()
        yield path, data.decode("utf-8", "replace")


def main() -> int:
    # Windows 控制台默认 GBK，打印中文/emoji 会 UnicodeEncodeError；CI 是 UTF-8 不受影响。
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")

    parser = argparse.ArgumentParser(description="密钥扫描（规则唯一来源）")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--all", action="store_true", help="扫 git 跟踪的全部文件（CI）")
    mode.add_argument("--staged", action="store_true", help="只扫暂存区（pre-commit）")
    mode.add_argument("--path", help="扫指定文件或目录（本地调试）")
    args = parser.parse_args()

    if os.environ.get("ALLOW_SECRET", "").strip().lower() in ("1", "true", "yes"):
        print("::warning::ALLOW_SECRET=1：本次跳过密钥扫描（仅用于误报放行，别当习惯）", file=sys.stderr)
        return 0

    repo = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], check=True, capture_output=True, text=True
    ).stdout.strip()

    hits: list[tuple[str, int, str, str]] = []
    scanned = 0
    if args.all:
        source = iter_tracked(repo)
    elif args.staged:
        source = iter_staged(repo)
    else:
        source = iter_path(args.path)

    for rel, text in source:
        scanned += 1
        scan_text(rel, text, hits)

    if not hits:
        print(f"secret-scan: {scanned} 个文件，0 命中 ✅")
        return 0

    print(f"secret-scan: {scanned} 个文件，命中 {len(hits)} 处 ❌", file=sys.stderr)
    for rule, lineno, path, masked in hits:
        message = f"[{rule}] 疑似密钥（已脱敏）：{masked}"
        print(f"  {path}:{lineno}: {message}", file=sys.stderr)
        if os.environ.get("GITHUB_ACTIONS") == "true":
            print(f"::error file={path},line={lineno}::{message}")
    print(
        "处理方式：把真实密钥换成环境变量或 sk-test- 开头的示例值；"
        "确属误报可 `ALLOW_SECRET=1` 放行（会打警告）。",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
