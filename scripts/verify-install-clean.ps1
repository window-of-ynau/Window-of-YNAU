#!/usr/bin/env pwsh
# 干净环境安装验证：用改写后的 LOCALAPPDATA 启动已安装的 App，证明它在一个"空的可写根"里
# 从头到尾能起来、且**不碰真实用户目录**；卸载后用户数据仍保留。
#
# 为什么用环境变量而不是沙盒/虚拟机：
#   Windows 11 **家庭版**没有 Windows Sandbox（Containers-DisposableClientVM）也没有 Hyper-V
#   （需专业版/企业版）。而应用的数据根来自 `LOCALAPPDATA ?? APPDATA ?? USERPROFILE`
#   （app/lib/src/core/app_paths.dart），所以给进程一个改写的 LOCALAPPDATA 就等于给它一个空的可写根。
#   ⚠️ 这不是安全边界：注册表、开始菜单快捷方式、Program Files 都还是真的那份。要"系统级干净"
#   请看 deploy/README.md「干净环境验证」里的另两种做法（新建本地 Windows 用户 / 干净虚拟机）。
#
# 用法：
#   pwsh scripts/verify-install-clean.ps1                       # 自动找 dist\WindowOfYNAU-*-setup.exe
#   pwsh scripts/verify-install-clean.ps1 -Setup .\dist\WindowOfYNAU-0.9.0-setup.exe
#   pwsh scripts/verify-install-clean.ps1 -Root D:\verify -KeepArtifacts
#   pwsh scripts/verify-install-clean.ps1 -SkipLaunch           # 只验证安装/卸载，不启动应用
#
# 退出码：0 = 全部断言通过；1 = 有断言失败（失败项会在最后汇总）。

[CmdletBinding()]
param(
    [string]$Setup,
    [string]$Root = "D:\verify",
    [int]$LaunchSeconds = 15,
    [switch]$SkipLaunch,
    [switch]$KeepArtifacts
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$profileDir = Join-Path $Root "profile-$stamp"
$appDir = Join-Path $Root "app-$stamp"
$results = New-Object System.Collections.Generic.List[object]

function Check([string]$name, [bool]$ok, [string]$detail = "") {
    $script:results.Add([pscustomobject]@{ Check = $name; Ok = $ok; Detail = $detail })
    $tag = if ($ok) { "PASS" } else { "FAIL" }
    Write-Host ("[{0}] {1} {2}" -f $tag, $name, $detail)
}

function Step([string]$text) { Write-Host "`n=== $text ===" -ForegroundColor Cyan }

# ---- 0. 找安装包 ----
if (-not $Setup) {
    $cand = Get-ChildItem (Join-Path $repo "dist") -Filter "WindowOfYNAU-*-setup.exe" -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $cand) { throw "dist\ 下没有 WindowOfYNAU-*-setup.exe；请先用 scripts/build-windows-installer.ps1 打包，或传 -Setup <路径>" }
    $Setup = $cand.FullName
}
$Setup = (Resolve-Path $Setup).Path
Write-Host "安装包：$Setup ($([math]::Round((Get-Item $Setup).Length/1MB,2)) MB)"

# 真实用户数据目录（验证前后必须完全不变）
$realDataDir = Join-Path $env:LOCALAPPDATA "yunong"
function Get-DirFingerprint([string]$p) {
    if (-not (Test-Path $p)) { return "absent" }
    $files = @(Get-ChildItem $p -Recurse -File -Force -ErrorAction SilentlyContinue)
    $sum = ($files | Measure-Object Length -Sum).Sum
    return ("{0} files / {1} bytes / {2}" -f $files.Count, $sum, (Get-Item $p).LastWriteTimeUtc.ToString("o"))
}
$realBefore = Get-DirFingerprint $realDataDir
Write-Host "真实数据目录（验证前）：$realDataDir → $realBefore"

New-Item -ItemType Directory -Force -Path $profileDir | Out-Null
Write-Host "假用户配置目录：$profileDir"
Write-Host "安装目标目录：$appDir"

try {
    # ---- 1. 静默安装 ----
    Step "1) 静默安装"
    Start-Process -Wait -FilePath $Setup -ArgumentList @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-", "/DIR=$appDir")
    Check "安装后 yunong.exe 存在" (Test-Path (Join-Path $appDir "yunong.exe"))
    Check "安装后 yunong_core.dll 存在" (Test-Path (Join-Path $appDir "yunong_core.dll"))
    Check "安装后 flutter_windows.dll 存在" (Test-Path (Join-Path $appDir "flutter_windows.dll"))
    Check "安装后 data\ 存在" (Test-Path (Join-Path $appDir "data"))
    $unins = Get-ChildItem $appDir -Filter "unins*.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
    Check "安装后卸载程序存在" ($null -ne $unins)

    $forbidden = @(Get-ChildItem $appDir -Recurse -Force -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like "*.WebView2" -or $_.Name -eq "EBWebView" -or $_.Name -like "*.db" -or $_.Name -eq "secrets" })
    Check "安装目录无 .WebView2 / *.db / secrets" ($forbidden.Count -eq 0) (($forbidden | ForEach-Object { $_.Name }) -join ", ")

    # ---- 2. 用改写的 LOCALAPPDATA 启动 ----
    if (-not $SkipLaunch) {
        Step "2) 用改写的 LOCALAPPDATA 启动应用"
        $savedLocalAppData = $env:LOCALAPPDATA
        $proc = $null
        try {
            $env:LOCALAPPDATA = $profileDir
            Write-Host "  LOCALAPPDATA=$profileDir"
            $proc = Start-Process -FilePath (Join-Path $appDir "yunong.exe") -PassThru
            Start-Sleep -Seconds $LaunchSeconds
            Check "进程仍在运行（没有启动即崩）" (-not $proc.HasExited) ("pid=" + $proc.Id)
        } finally {
            $env:LOCALAPPDATA = $savedLocalAppData
        }
        if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue; Start-Sleep -Seconds 2 }

        Start-Sleep -Seconds 1
        $fakeData = Join-Path $profileDir "yunong"
        Check "假用户配置下创建了 yunong\ 数据目录" (Test-Path $fakeData) $fakeData
        if (Test-Path $fakeData) {
            $files = @(Get-ChildItem $fakeData -Recurse -File -Force -ErrorAction SilentlyContinue)
            Write-Host ("  数据目录内容：{0} 个文件" -f $files.Count)
            $files | Select-Object -First 12 | ForEach-Object { Write-Host ("    " + $_.FullName.Substring($fakeData.Length + 1)) }
        }

        $realAfter = Get-DirFingerprint $realDataDir
        Check "真实 %LOCALAPPDATA%\yunong 未被修改" ($realBefore -eq $realAfter) ("before=$realBefore / after=$realAfter")

        # 应用运行后，WebView2 默认会把用户数据放在 exe 同级——这是 t154 的 P0 根因，
        # 也是"装到 Program Files 后不可写 → 登录失败"的功能风险。这里断言它没有发生。
        $runtimeJunk = @(Get-ChildItem $appDir -Recurse -Force -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -like "*.WebView2" -or $_.Name -eq "EBWebView" -or $_.Name -like "*.db" -or $_.Name -eq "secrets" })
        Check "运行后安装目录仍无 .WebView2 / *.db / secrets" ($runtimeJunk.Count -eq 0) (($runtimeJunk | ForEach-Object { $_.Name }) -join ", ")
    } else {
        Write-Host "`n（-SkipLaunch：跳过启动验证）"
    }

    # ---- 3. 静默卸载 ----
    Step "3) 静默卸载"
    if ($unins) {
        Start-Process -Wait -FilePath $unins.FullName -ArgumentList @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART")
        Start-Sleep -Seconds 2
        Check "卸载后安装目录里的 yunong.exe 已删除" (-not (Test-Path (Join-Path $appDir "yunong.exe")))
        $fakeData = Join-Path $profileDir "yunong"
        Check "卸载后用户数据仍保留（我们承诺过不删）" (Test-Path $fakeData) $fakeData
        $realAfterUninstall = Get-DirFingerprint $realDataDir
        Check "卸载后真实 %LOCALAPPDATA%\yunong 仍未被修改" ($realBefore -eq $realAfterUninstall) ("before=$realBefore / after=$realAfterUninstall")
    } else {
        Check "卸载程序存在（跳过卸载）" $false
    }
} finally {
    # ---- 4. 结果汇总与清理 ----
    Step "结果汇总"
    $failed = @($results | Where-Object { -not $_.Ok })
    $results | Format-Table -AutoSize | Out-String | Write-Host
    if (-not $KeepArtifacts) {
        Write-Host "清理临时目录（-KeepArtifacts 可保留现场）"
        foreach ($p in @($profileDir, $appDir)) {
            if (Test-Path $p) {
                # 先清只读属性（安装目录里的文件可能是只读）
                Get-ChildItem $p -Recurse -Force -ErrorAction SilentlyContinue | ForEach-Object { $_.Attributes = "Normal" } 2>$null
                Remove-Item $p -Recurse -Force -ErrorAction SilentlyContinue
            }
        }
    } else {
        Write-Host "保留现场：$profileDir / $appDir"
    }
    if ($failed.Count -eq 0) {
        Write-Host "`n✅ 全部通过（$($results.Count) 项）" -ForegroundColor Green
        exit 0
    } else {
        Write-Host "`n❌ 失败 $($failed.Count) 项：" -ForegroundColor Red
        $failed | ForEach-Object { Write-Host ("  - " + $_.Check + " " + $_.Detail) }
        exit 1
    }
}
