#!/usr/bin/env pwsh
# 为子代理创建独立工作树，避免并发写入冲突。
# 用法：pwsh scripts/new-agent-worktree.ps1 -Name rust-parser [-Base main]
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9][a-z0-9-]*$')]
    [string]$Name,

    [string]$Base = 'main'
)

$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$worktreeRoot = Join-Path $repo '.worktrees'
$path = Join-Path $worktreeRoot $Name
$branch = "agent/$Name"

if (-not (Test-Path (Join-Path $repo '.git'))) {
    throw "当前目录不是 git 仓库：$repo（请先 git init 并至少提交一次）"
}

$dirty = git -C $repo status --porcelain
if ($dirty) {
    Write-Warning "主工作树有未提交改动，工作树会从 $Base 的提交分出，看不到这些改动。"
}

if (Test-Path $path) {
    throw "工作树已存在：$path"
}

New-Item -ItemType Directory -Force -Path $worktreeRoot | Out-Null

$existingBranches = git -C $repo branch --list $branch
if ($existingBranches) {
    git -C $repo worktree add $path $branch
} else {
    git -C $repo worktree add -b $branch $path $Base
}

Write-Host ""
Write-Host "已创建：" -NoNewline
Write-Host $path -ForegroundColor Green
Write-Host "分支：  $branch（基于 $Base）"
Write-Host ""
Write-Host "派活时请把这段写进子代理提示词：" -ForegroundColor Cyan
Write-Host "  你的工作目录是 $path（git worktree，分支 $branch）。"
Write-Host "  只允许修改该目录下的文件；不要切换或写入其他工作树。"
Write-Host "  完成后在该目录内 git add -A && git commit，并回报分支名 / commit hash / 改动清单。"
Write-Host ""
