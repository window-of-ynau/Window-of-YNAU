#!/usr/bin/env sh
# 安装 git pre-commit 钩子：提交前跑密钥扫描，规则只有一份（scripts/secret-scan.py）。
#
# 用法：
#   bash scripts/install-hooks.sh          # Linux / macOS / Git Bash
#   powershell -File scripts/install-hooks.ps1   # Windows PowerShell
#
# 误报放行：ALLOW_SECRET=1 git commit -m "..."（会打警告，别当习惯）
set -eu

root=$(git rev-parse --show-toplevel)
hooks_dir=$(git rev-parse --git-path hooks)
case "$hooks_dir" in
    /* | [A-Za-z]:*) ;;
    *) hooks_dir="$root/$hooks_dir" ;;
esac

mkdir -p "$hooks_dir"
hook="$hooks_dir/pre-commit"

cat > "$hook" <<'HOOK'
#!/bin/sh
# 由 scripts/install-hooks.sh（或 install-hooks.ps1）安装，别手改——
# 规则在 scripts/secret-scan.py，CI 的 secret-scan job 用的是同一份。
# 误报放行：ALLOW_SECRET=1 git commit -m "..."
ROOT=$(git rev-parse --show-toplevel)
if command -v python3 >/dev/null 2>&1; then
    PY=python3
elif command -v python >/dev/null 2>&1; then
    PY=python
else
    echo "secret-scan: 找不到 python3/python，跳过密钥扫描（请装 Python 3）" >&2
    exit 0
fi
exec "$PY" "$ROOT/scripts/secret-scan.py" --staged
HOOK

chmod +x "$hook" 2>/dev/null || true
echo "已安装 pre-commit 钩子：$hook"
echo "自测：把一行 'sk-' + 20 位以上字符加进暂存区再 commit，应当被拦；示例值用 sk-test- 开头。"
