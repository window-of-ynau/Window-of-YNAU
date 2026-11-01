#!/usr/bin/env pwsh
# 合并 agent 分支前的守门脚本：先审、再验、后合。
#
# 用法：
#   pwsh scripts/merge-agent.ps1 -Name auth              # 审阅 + 校验 + 合并（必要时自动 rebase）
#   pwsh scripts/merge-agent.ps1 -Name auth -DryRun     # 只看不合并
#   pwsh scripts/merge-agent.ps1 -Name auth -Force      # 放行单写者告警
#
# 为什么用 merge-base 而不是直接 diff main..branch：
# agent 分支是从某个历史提交分出去的；分支落后于 main 时，`main..branch` 会把
# **我们后来的提交显示成回退**（曾把 docs 与脚本判成"被删除"）。用 merge-base 才对。
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Name,

    [switch]$DryRun,
    [switch]$Force,
    # 不自动 rebase（默认会 rebase 到最新 main 再校验）
    [switch]$NoRebase
)

$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$worktree = Join-Path $repo ".worktrees/$Name"
$branch = "agent/$Name"

if (-not (Test-Path $worktree)) {
    throw "找不到工作树：$worktree（先跑 scripts/new-agent-worktree.ps1 -Name $Name）"
}
# 注意：`git show-ref --quiet` 成功时不输出任何内容，所以必须看 $LASTEXITCODE，
# 不能用 `-not (git ...)` 判断（那样会永远判成"不存在"）。
git -C $repo show-ref --verify --quiet "refs/heads/$branch"
if ($LASTEXITCODE -ne 0) {
    throw "找不到分支：$branch"
}

Write-Host "== 0/5 基线同步 ==" -ForegroundColor Cyan
$behind = [int](git -C $repo rev-list --count "main..$branch" 2>$null)
$aheadOf = [int](git -C $repo rev-list --count "$branch..main" 2>$null)
Write-Host "分支领先 main：$behind 个提交；main 领先分支：$aheadOf 个提交"
if ($aheadOf -gt 0) {
    if ($NoRebase) {
        Write-Warning "分支落后 $aheadOf 个提交且要求不 rebase：差异里会混入'回退'，请自行确认。"
    } else {
        Write-Host "→ rebase 到最新 main ..."
        git -C $worktree rebase main
        if ($LASTEXITCODE -ne 0) { throw "rebase 冲突：在 $worktree 里解决后 git rebase --continue" }
    }
}

$base = (git -C $repo merge-base main $branch).Trim()
Write-Host "比较基线（merge-base）：$base"

Write-Host "`n== 1/5 变更概览 ==" -ForegroundColor Cyan
git -C $repo --no-pager diff --stat "$base..$branch"
$changed = @(git -C $repo diff --name-only "$base..$branch")
Write-Host "文件数：$($changed.Count)"

Write-Host "`n== 2/5 单写者检查 ==" -ForegroundColor Cyan
$singleWriter = @(
    '^crates/shared/',
    '^Cargo\.toml$',
    '^Cargo\.lock$',   # lock 变了就是对依赖树的实质改动，必须可见
    '^docs/04-ADR\.md$',
    '^docs/0[123]-',
    '^\.github/',
    '^app/pubspec\.yaml$'
)
$violations = @($changed | Where-Object {
        $path = $_
        $singleWriter | Where-Object { $path -match $_ }
    })
if ($violations.Count -gt 0) {
    Write-Warning "agent 动了单写者文件："
    $violations | ForEach-Object { Write-Warning "  - $_" }
    if (-not $Force) { throw "请人工确认（确认后加 -Force），或让 agent 撤回。" }
} else {
    Write-Host "OK：未触碰单写者文件"
}

Write-Host "`n== 3/5 质量门禁（在 agent 工作树内跑）==" -ForegroundColor Cyan
Push-Location $worktree
try {
    $env:CARGO_NET_OFFLINE = 'true'
    # 默认给每个工作树独立 target：共享目录曾出现陈旧指纹，导致测试数虚高
    if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $worktree 'target' }

    cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw "fmt 未通过（先 cargo fmt --all）" }

    cargo clippy --workspace --all-targets --offline -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "clippy 未通过" }

    cargo test --workspace --offline
    if ($LASTEXITCODE -ne 0) { throw "测试未通过" }

    if (Test-Path (Join-Path $worktree 'scripts/check_docs.py')) {
        python scripts/check_docs.py docs
        if ($LASTEXITCODE -ne 0) { throw "文档校验未通过" }
    }
} finally {
    Pop-Location
}

Write-Host "`n== 4/5 合并 ==" -ForegroundColor Cyan
if ($DryRun) {
    Write-Host "DryRun：跳过合并。确认无误后去掉 -DryRun。" -ForegroundColor Yellow
    return
}
$dirty = git -C $repo status --porcelain
if ($dirty) { throw "主工作树有未提交改动，先提交或 stash。" }

git -C $repo merge --no-ff $branch -m "merge: $branch（经 scripts/merge-agent.ps1 校验）"
if ($LASTEXITCODE -ne 0) { throw "合并冲突：在主树解决后 git merge --continue" }

Write-Host "`n== 5/5 合并后在 main 复验 ==" -ForegroundColor Cyan
Push-Location $repo
try {
    $env:CARGO_TARGET_DIR = Join-Path $repo 'target'
    cargo test --workspace --offline
    if ($LASTEXITCODE -ne 0) { throw "main 上复验失败（已合并，需修复后补提交）" }
} finally {
    Pop-Location
}

Write-Host "`n✅ 已合并 $branch。" -ForegroundColor Green
Write-Host "收尾（确认无误后）：git worktree remove .worktrees/$Name ; git branch -d $branch ; git push"
