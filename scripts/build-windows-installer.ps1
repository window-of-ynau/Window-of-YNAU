#!/usr/bin/env pwsh
# 打 Windows 安装包（Inno Setup）：构建应用产物 → **白名单暂存** → ISCC 编译 setup.exe。
#
# ⚠️ 为什么不再直接拿 Release 目录打包（t154 P0 事故）：
#   WebView2 在**运行 App 时**会在 exe 同级创建 `yunong.exe.WebView2/EBWebView/`，里面是
#   本机的 Cookie、Login Data、缓存、Crashpad。之前 .iss 用 `{SourceDir}\*` 通配打包，
#   等于把这些用户数据打进了发布安装包。现在改成三道：
#     A) 打包前断言 Release 目录里**没有** WebView2 数据/数据库/密钥/调试符号；
#     B) 只把白名单文件复制到 build/installer-staging（干净暂存），.iss 只指向它；
#     C) 打包后断言体积 < 40 MB（异常增大往往是混进了不该有的东西）。
#
# 用法：
#   pwsh scripts/build-windows-installer.ps1                      # 构建 + 打包，版本取自 Cargo.toml
#   pwsh scripts/build-windows-installer.ps1 -Version 0.1.0
#   pwsh scripts/build-windows-installer.ps1 -SkipAppBuild        # 复用已有产物
#   pwsh scripts/build-windows-installer.ps1 -ReleaseDir <dir>    # 指定产物目录（复现/测试用）
#
# 产物：dist/WindowOfYNAU-<version>-setup.exe（+ SHA256SUMS）

[CmdletBinding()]
param(
    [string]$Version,
    [string]$OutputDir = "dist",
    [string]$ReleaseDir,
    [switch]$SkipAppBuild
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
if (-not $ReleaseDir) { $ReleaseDir = Join-Path $repo "app/build/windows/x64/runner/Release" }
$stagingDir = Join-Path $repo "build/installer-staging"

function Step([string]$text) { Write-Host "`n=== $text ===" -ForegroundColor Cyan }

if (-not $Version) {
    $cargoToml = Get-Content (Join-Path $repo "Cargo.toml") -Raw
    if ($cargoToml -match '(?m)^version\s*=\s*"([^"]+)"') {
        $Version = $Matches[1]
    } else {
        throw "读不到 Cargo.toml 的 [workspace.package].version，请显式传 -Version"
    }
}
Write-Host "安装包版本：$Version"

# ---- 1. 应用产物 ----
if (-not $SkipAppBuild) {
    & (Join-Path $PSScriptRoot "build-windows-app.ps1")
    if ($LASTEXITCODE -ne 0) { throw "build-windows-app.ps1 失败" }
}

# ---- 2. 护栏 A：产物目录里不许有用户数据 / 密钥 / 调试符号 ----
Step "护栏 A：Release 目录不得含 WebView2 用户数据 / *.db / secrets / *.pdb / .env"
if (-not (Test-Path $ReleaseDir)) { throw "产物目录不存在：$ReleaseDir" }

$forbidden = @(Get-ChildItem $ReleaseDir -Recurse -Force -ErrorAction SilentlyContinue |
    Where-Object {
        $_.Name -like "*.WebView2" -or $_.Name -eq "EBWebView" -or
        $_.Name -like "*.db" -or $_.Name -like "*.db-journal" -or $_.Name -like "*.db-wal" -or $_.Name -like "*.db-shm" -or
        $_.Name -eq "secrets" -or $_.Name -like "*.pdb" -or $_.Name -eq ".env" -or
        $_.Name -like "*.jks" -or $_.Name -like "*.keystore"
    })
if ($forbidden.Count -gt 0) {
    $list = ($forbidden | Select-Object -First 20 | ForEach-Object { "  " + $_.FullName.Substring($ReleaseDir.Length + 1) }) -join "`n"
    throw @"
产物目录里发现不该发布的东西（共 $($forbidden.Count) 项）：
$list

这通常是在 Release 目录里**跑过一次 App** 留下的（WebView2 用户数据含 Cookie / Login Data）。
处理：删掉这些文件后重跑，或在干净目录重新 `flutter build windows`。
"@
}
Write-Host "OK：Release 目录干净（无 WebView2 / 数据库 / 密钥 / 调试符号）"

# ---- 3. 护栏 B：白名单暂存 ----
Step "护栏 B：白名单复制到暂存目录（默认排除一切未列出的文件）"
if (Test-Path $stagingDir) { Remove-Item $stagingDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $stagingDir | Out-Null

# 白名单：Flutter Windows 运行时必需 + 端侧核心 + WebView2 加载器。
# 以后加了新的插件 DLL，必须显式加到这里，否则装上去会缺 DLL。
$whitelistFiles = @(
    "yunong.exe",
    "yunong_core.dll",
    "flutter_windows.dll",
    "flutter_inappwebview_windows_plugin.dll",
    "WebView2Loader.dll",
    "native_assets.json"
)

foreach ($f in $whitelistFiles) {
    $src = Join-Path $ReleaseDir $f
    if (Test-Path $src) {
        Copy-Item $src (Join-Path $stagingDir $f) -Force
        Write-Host ("  复制 {0} ({1:N1} MB)" -f $f, ((Get-Item $src).Length / 1MB))
    } elseif ($f -ne "native_assets.json") {
        throw "白名单文件在产物目录里找不到：$f"
    }
}

# data/ 整目录（Flutter 资源），排除数据库/密钥类
$dataSrc = Join-Path $ReleaseDir "data"
if (-not (Test-Path $dataSrc)) { throw "缺少 data\ 目录：$dataSrc" }
$dataExclude = @("*.db", "*.db-journal", "*.db-wal", "*.db-shm", "secrets", ".env", "*.pdb")
robocopy $dataSrc (Join-Path $stagingDir "data") /E /NFL /NDL /NJH /NJS /NP /XF @dataExclude | Out-Null
if ($LASTEXITCODE -ge 8) { throw "robocopy 复制 data\ 失败（exit $LASTEXITCODE）" }
Write-Host "  复制 data\（排除：$($dataExclude -join ', ')）"

# 暂存目录再断言一次（纵深防御）
$leaked = @(Get-ChildItem $stagingDir -Recurse -Force -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -like "*.WebView2" -or $_.Name -eq "EBWebView" -or $_.Name -like "*.db" -or
                   $_.Name -eq "secrets" -or $_.Name -like "*.pdb" -or $_.Name -eq ".env" })
if ($leaked.Count -gt 0) {
    throw "暂存目录里仍有不该发布的内容：$(($leaked | ForEach-Object { $_.Name }) -join ', ')"
}
$stagingFiles = @(Get-ChildItem $stagingDir -Recurse -File)
$stagingMb = [math]::Round((($stagingFiles | Measure-Object Length -Sum).Sum / 1MB), 2)
Write-Host ("暂存目录：{0} 个文件，{1} MB" -f $stagingFiles.Count, $stagingMb)

# ---- 4. 找 ISCC 并编译 ----
Step "编译安装包"
$iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue)?.Source
if (-not $iscc) {
    # 自定义安装目录很常见（本机就装在 D:\Program Files\Inno Setup 6）：先查注册表的 InstallLocation
    foreach ($key in @(
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*",
        "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*",
        "HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*"
    )) {
        $entry = Get-ItemProperty $key -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName -like "*Inno Setup*" } | Select-Object -First 1
        if ($entry -and $entry.InstallLocation) {
            $cand = Join-Path $entry.InstallLocation "ISCC.exe"
            if (Test-Path $cand) { $iscc = $cand; break }
        }
    }
}
if (-not $iscc) {
    foreach ($p in @(
        "C:\Program Files (x86)\Inno Setup 6\ISCC.exe",
        "C:\Program Files\Inno Setup 6\ISCC.exe",
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "D:\Program Files\Inno Setup 6\ISCC.exe"
    )) { if (Test-Path $p) { $iscc = $p; break } }
}
if (-not $iscc) {
    throw "找不到 ISCC.exe：请安装 Inno Setup 6（winget install JRSoftware.InnoSetup / CI 用 choco install innosetup -y）"
}
Write-Host "ISCC：$iscc"

$outDir = if ([System.IO.Path]::IsPathRooted($OutputDir)) { $OutputDir } else { Join-Path $repo $OutputDir }
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$iss = Join-Path $repo "deploy/windows/installer.iss"

# t160：安装向导/卸载列表的图标 = 应用图标。用**绝对路径**传进去，不依赖 .iss 的相对路径基准
# （.iss 会在 deploy/windows/ 下被编译，但将来若有人把 iss 复制到暂存目录，相对路径就废了）。
$iconPath = Join-Path $repo "app/windows/runner/resources/app_icon.ico"
if (-not (Test-Path $iconPath)) { throw "找不到应用图标：$iconPath" }
Write-Host "图标：$iconPath ($([math]::Round((Get-Item $iconPath).Length/1KB,1)) KB)"

& $iscc "/DMyAppVersion=$Version" "/DSourceDir=$stagingDir" "/DOutputDir=$outDir" "/DSetupIconFile=$iconPath" "/Qp" $iss
if ($LASTEXITCODE -ne 0) { throw "ISCC 失败（exit $LASTEXITCODE）" }

# ---- 5. 护栏 C：体积与校验和 ----
$setup = Join-Path $outDir "WindowOfYNAU-$Version-setup.exe"
if (-not (Test-Path $setup)) { throw "没找到产物：$setup" }
$sizeMb = [math]::Round((Get-Item $setup).Length / 1MB, 2)
Write-Host "`nOK: $setup ($sizeMb MB)"
if ($sizeMb -ge 40) {
    throw "安装包体积异常：$sizeMb MB（阈值 40 MB）——检查暂存目录是否混入了用户数据/调试符号。"
}

# 护栏 D（t160）：安装包图标必须是自己那张 ICO，不能是 Inno 默认图标。
# 做法：从 setup.exe 与 yunong.exe 各取出"关联图标"，比对像素哈希；不一致就说明 SetupIconFile 没生效。
Add-Type -AssemblyName System.Drawing
function Get-IconHash([string]$exePath) {
    $icon = [System.Drawing.Icon]::ExtractAssociatedIcon($exePath)
    if (-not $icon) { return $null }
    $bmp = $icon.ToBitmap()
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $hash = [BitConverter]::ToString($sha.ComputeHash($ms.ToArray())) -replace '-', ''
    $ms.Dispose(); $bmp.Dispose(); $icon.Dispose()
    return $hash
}
$setupIconHash = Get-IconHash $setup
$appExe = Join-Path $stagingDir "yunong.exe"
$appIconHash = if (Test-Path $appExe) { Get-IconHash $appExe } else { $null }
if (-not $setupIconHash) { throw "护栏 D：读不出 setup.exe 的图标资源" }
if ($appIconHash -and ($setupIconHash -ne $appIconHash)) {
    throw "护栏 D：setup.exe 的图标与应用不一致（setup=$setupIconHash app=$appIconHash）——SetupIconFile 可能没生效"
}
Write-Host "护栏 D OK：setup.exe 图标 = 应用图标（sha256=${setupIconHash}）"

Push-Location $outDir
try {
    Get-ChildItem -Filter "*.exe" | Get-FileHash -Algorithm SHA256 |
        ForEach-Object { "$($_.Hash.ToLower())  $([System.IO.Path]::GetFileName($_.Path))" } |
        Set-Content -Path "SHA256SUMS" -Encoding utf8NoBOM
    Get-Content "SHA256SUMS"
} finally { Pop-Location }
