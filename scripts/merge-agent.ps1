#!/usr/bin/env pwsh
# 合并 agent 分支前的守门脚本：先审、再验、后合。
# 用法：pwsh scripts/merge-agent.ps1 -Name auth [-DryRun] [-Force]
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Name,

    # 只看差异与校验结果，不真的合并
    [switch]$DryRun,

    # 跳过"单写者文件"告警（仍会跑校验）
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$worktree = Join-Path $repo ".worktrees/$Name"
$branch = "agent/$Name"

if (-not (Test-Path $worktree)) {
    throw "找不到工作树：$worktree（先跑 scripts/new-agent-worktree.ps1 -Name $Name）"
}
if (-not (git -C $repo show-ref --verify --quiet "refs/heads/$branch")) {
    throw "找不到分支：$branch"
}

Write-Host "== 1/4 变更概览 ==" -ForegroundColor Cyan
git -C $repo --no-pager diff --stat "main..$branch"
$changed = @(git -C $repo diff --name-only "main..$branch")
Write-Host "文件数：$($changed.Count)"

Write-Host "`n== 2/4 单写者检查 ==" -ForegroundColor Cyan
# 这些文件是全局契约，agent 不应自行改动（docs/integrations/agent-worktrees.md §3）
$singleWriter = @(
    '^crates/shared/',
    '^Cargo\.toml$',
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
    if (-not $Force) {
        throw "请先人工确认这些改动（确认后加 -Force 重跑），或让 agent 撤回。"
    }
} else {
    Write-Host "OK：未触碰单写者文件"
}

Write-Host "`n== 3/4 质量门禁（在 agent 工作树内跑）==" -ForegroundColor Cyan
Push-Location $worktree
try {
    $env:CARGO_NET_OFFLINE = 'true'
    if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path (Split-Path $repo -Parent) 'yunong-target' }

    cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw "fmt 未通过（请 agent 先 cargo fmt）" }

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

Write-Host "`n== 4/4 合并 ==" -ForegroundColor Cyan
if ($DryRun) {
    Write-Host "DryRun：跳过合并。确认无误后去掉 -DryRun 重跑。" -ForegroundColor Yellow
    return
}

$dirty = git -C $repo status --porcelain
if ($dirty) {
    throw "主工作树有未提交改动，先提交或 stash 再合并。"
}

git -C $repo merge --no-ff $branch -m "merge: $branch（经 scripts/merge-agent.ps1 校验）"
if ($LASTEXITCODE -ne 0) {
    throw "合并冲突：请在主树解决后 git merge --continue"
}

Write-Host "`n已合并 $branch。" -ForegroundColor Green
Write-Host "收尾（确认无误后再执行）："
Write-Host "  git worktree remove .worktrees/$Name"
Write-Host "  git branch -d $branch"
