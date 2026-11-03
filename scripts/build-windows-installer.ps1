#!/usr/bin/env pwsh
# 打 Windows 安装包（Inno Setup）：先构建应用产物，再交给 ISCC 编译 setup.exe。
#
# 用法：
#   pwsh scripts/build-windows-installer.ps1                        # 版本取自 Cargo.toml
#   pwsh scripts/build-windows-installer.ps1 -Version 0.1.0
#   pwsh scripts/build-windows-installer.ps1 -SkipAppBuild          # 复用已有产物（CI 分步时用）
#
# 产物：dist/WindowOfYNAU-<version>-setup.exe（+ SHA256SUMS）
#
# 前置：Inno Setup 6（CI 用 `choco install innosetup -y`；本机 `winget install JRSoftware.InnoSetup`）。

[CmdletBinding()]
param(
    [string]$Version,
    [string]$OutputDir = "dist",
    [switch]$SkipAppBuild
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$releaseDir = Join-Path $repo "app/build/windows/x64/runner/Release"

if (-not $Version) {
    $cargoToml = Get-Content (Join-Path $repo "Cargo.toml") -Raw
    if ($cargoToml -match '(?m)^version\s*=\s*"([^"]+)"') {
        $Version = $Matches[1]
    } else {
        throw "读不到 Cargo.toml 的 [workspace.package].version，请显式传 -Version"
    }
}
Write-Host "安装包版本：$Version"

# 1) 应用产物（exe + DLL 同目录 + data\）
if (-not $SkipAppBuild) {
    & (Join-Path $PSScriptRoot "build-windows-app.ps1")
    if ($LASTEXITCODE -ne 0) { throw "build-windows-app.ps1 失败" }
}

# 2) 打包前的硬校验：少一个 DLL 的安装包 = 用户装完打不开
$required = @("yunong.exe", "yunong_core.dll", "flutter_windows.dll")
$missing = @()
foreach ($f in $required) {
    if (-not (Test-Path (Join-Path $releaseDir $f))) { $missing += $f }
}
if (-not (Test-Path (Join-Path $releaseDir "data"))) { $missing += "data\" }
if ($missing.Count -gt 0) {
    throw "产物目录缺少：$($missing -join ', ')（目录：$releaseDir）"
}
Write-Host "产物校验通过：$($required -join ' + ') + data\"

# 3) 找 ISCC
$iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue)?.Source
if (-not $iscc) {
    foreach ($p in @(
        "C:\Program Files (x86)\Inno Setup 6\ISCC.exe",
        "C:\Program Files\Inno Setup 6\ISCC.exe",
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
    )) {
        if (Test-Path $p) { $iscc = $p; break }
    }
}
if (-not $iscc) {
    throw "找不到 ISCC.exe：请安装 Inno Setup 6（winget install JRSoftware.InnoSetup / CI 用 choco install innosetup -y）"
}
Write-Host "ISCC：$iscc"

# 4) 编译安装包
$outDir = if ([System.IO.Path]::IsPathRooted($OutputDir)) { $OutputDir } else { Join-Path $repo $OutputDir }
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$iss = Join-Path $repo "deploy/windows/installer.iss"
$isccArgs = @(
    "/DMyAppVersion=$Version",
    "/DSourceDir=$releaseDir",
    "/DOutputDir=$outDir",
    "/Qp",
    $iss
)
& $iscc @isccArgs
if ($LASTEXITCODE -ne 0) { throw "ISCC 失败（exit $LASTEXITCODE）" }

# 5) 校验和 + 大小
$setup = Join-Path $outDir "WindowOfYNAU-$Version-setup.exe"
if (-not (Test-Path $setup)) { throw "没找到产物：$setup" }
$sizeMb = [math]::Round((Get-Item $setup).Length / 1MB, 1)
Write-Host "`nOK: $setup ($sizeMb MB)"
Push-Location $outDir
try {
    Get-ChildItem -Filter "*.exe" | Get-FileHash -Algorithm SHA256 |
        ForEach-Object { "$($_.Hash.ToLower())  $([System.IO.Path]::GetFileName($_.Path))" } |
        Set-Content -Path "SHA256SUMS" -Encoding utf8NoBOM
    Get-Content "SHA256SUMS"
} finally { Pop-Location }
