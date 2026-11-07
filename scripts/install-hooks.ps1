# 安装 git pre-commit 钩子（Windows PowerShell）：提交前跑密钥扫描。
#
# 规则只有一份：scripts/secret-scan.py（CI 的 secret-scan job 共用）。
# 误报放行：$env:ALLOW_SECRET=1; git commit -m "..."（会打警告）
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts/install-hooks.ps1
#   pwsh -File scripts/install-hooks.ps1
$ErrorActionPreference = "Stop"

$root = (git rev-parse --show-toplevel).Trim()
$hooksDir = (git rev-parse --git-path hooks).Trim()
if (-not [System.IO.Path]::IsPathRooted($hooksDir)) {
    $hooksDir = Join-Path $root $hooksDir
}
New-Item -ItemType Directory -Force -Path $hooksDir | Out-Null

$hookPath = Join-Path $hooksDir "pre-commit"
$hook = @'
#!/bin/sh
# 由 scripts/install-hooks.ps1（或 install-hooks.sh）安装，别手改——
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
# fail-open：脚本不存在（例如切到还没有 scripts/secret-scan.py 的旧分支）时放行，
# 不要因为钩子引用了一个不存在的文件就把所有提交卡死。
SCAN="$ROOT/scripts/secret-scan.py"
if [ ! -f "$SCAN" ]; then
    echo "secret-scan: 找不到 $SCAN，跳过扫描（fail-open）" >&2
    exit 0
fi
exec "$PY" "$SCAN" --staged
'@

# 必须写成 LF：Git for Windows 用 sh 执行钩子，CRLF 的 shebang 会报 "bad interpreter"。
$normalized = $hook.Replace("`r`n", "`n")
[System.IO.File]::WriteAllText($hookPath, $normalized, (New-Object System.Text.UTF8Encoding($false)))

Write-Host "已安装 pre-commit 钩子：$hookPath"
Write-Host "自测：把一行 'sk-' + 20 位以上字符加进暂存区再 commit，应当被拦；示例值用 sk-test- 开头。"
