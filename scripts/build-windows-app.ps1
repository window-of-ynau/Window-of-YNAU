#!/usr/bin/env pwsh
# 一键打包 Windows 客户端（Flutter UI + Rust 端侧核心 DLL）。
#
# 为什么需要这个脚本：`flutter build windows` **不会**构建也不会复制 `yunong_core.dll`。
# 少了它，App 能启动但 FFI 找不到库，界面上只会显示"端侧核心未就绪"——
# 看起来像功能没做，其实是打包漏了。这个脚本把两件事绑在一起做。
#
# 用法：
#   pwsh scripts/build-windows-app.ps1              # release
#   pwsh scripts/build-windows-app.ps1 -Debug       # debug（快一些）
#   pwsh scripts/build-windows-app.ps1 -SkipFlutter # 只重编 DLL 并复制到已有产物目录

[CmdletBinding()]
param(
    [switch]$Debug,
    [switch]$SkipFlutter
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$profile = if ($Debug) { "debug" } else { "release" }

function Step([string]$text) { Write-Host "`n=== $text ===" -ForegroundColor Cyan }

# 1) 前置检查：flutter_inappwebview 的 Windows 实现用 nuget 拉 nlohmann.json，
#    没有它构建会在 MSB3073 / NUGET-NOTFOUND 处失败，报错信息很不直观。
if (-not (Get-Command nuget -ErrorAction SilentlyContinue)) {
    throw @"
找不到 nuget.exe（flutter_inappwebview 的 Windows 构建依赖它）。

  winget install Microsoft.NuGet
  或手动下载 https://dist.nuget.org/win-x86-commandline/latest/nuget.exe
  放到 %LOCALAPPDATA%\Programs\nuget 并加入 PATH，然后重开终端。

没装的话构建会在 MSB3073 "NUGET-NOTFOUND install nlohmann.json" 处失败。
"@
}

# 2) 端侧核心：cdylib
Step "构建 Rust 端侧核心（$profile）"
Push-Location $repo
try {
    cargo build -p yunong-core --$(if ($Debug) { 'debug' } else { 'release' })
    if ($LASTEXITCODE -ne 0) { throw "cargo build 失败" }
} finally { Pop-Location }

$coreName = if ($IsWindows -or $env:OS -eq "Windows_NT") { "yunong_core.dll" }
            elseif ($IsMacOS) { "libyunong_core.dylib" }
            else { "libyunong_core.so" }
$corePath = Join-Path $repo "target/$profile/$coreName"
if (-not (Test-Path $corePath)) { throw "没找到端侧核心：$corePath" }

# 3) Flutter 产物
$releaseDir = Join-Path $repo "app/build/windows/x64/runner/$(if ($Debug) { 'Debug' } else { 'Release' })"
if (-not $SkipFlutter) {
    Step "构建 Flutter Windows（$profile）"
    Push-Location (Join-Path $repo "app")
    try {
        flutter build windows --$profile
        if ($LASTEXITCODE -ne 0) { throw "flutter build windows 失败" }
    } finally { Pop-Location }
}

if (-not (Test-Path $releaseDir)) { throw "没有产物目录：$releaseDir" }

# 4) 把 DLL 放到 exe 旁边（Dart 侧按 exe 目录查找）
Step "打包 DLL"
Copy-Item $corePath (Join-Path $releaseDir $coreName) -Force
$size = [math]::Round((Get-Item (Join-Path $releaseDir $coreName)).Length / 1MB, 2)

Write-Host "`n✅ 完成" -ForegroundColor Green
Write-Host "   产物：$releaseDir"
Write-Host "   核心：$coreName（$size MB）"
Write-Host "   启动：$releaseDir\yunong.exe"
Write-Host "`n提示：若界面提示「端侧核心未就绪」，先确认 $coreName 与 yunong.exe 在同一目录。" -ForegroundColor Yellow
