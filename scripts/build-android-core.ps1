#!/usr/bin/env pwsh
# 交叉编译 Rust 端侧核心（yunong-core）为 Android 的 libyunong_core.so，并放到
# app/android/app/src/main/jniLibs/<abi>/ 供 APK 打包。
#
# 为什么需要这个脚本：`flutter build apk` **不会**编 Rust 的 cdylib。缺了 .so 的 APK
# 装上去能启动，但 FFI 加载失败，界面只会显示"端侧核心未就绪"。
#
# 用法：
#   pwsh scripts/build-android-core.ps1                     # 默认 arm64-v8a（release）
#   pwsh scripts/build-android-core.ps1 -Abi arm64-v8a,x86_64
#   pwsh scripts/build-android-core.ps1 -DebugBuild         # debug（快，体积大）
#   pwsh scripts/build-android-core.ps1 -DryRun             # 只打印将要执行的命令与环境
#   pwsh scripts/build-android-core.ps1 -Clean              # 先清掉旧 .so 与 jniLibs 目录
#
# 依赖：NDK（默认找 ANDROID_NDK_HOME / ANDROID_HOME/ndk / $env:ANDROID_HOME\ndk 下最高版本），
# 以及 `rustup target add aarch64-linux-android`（脚本会检查并提示）。
#
# 不用 cargo-ndk：它只是把同样的 env 拼起来；手写清楚反而好排查（链接器路径、API level 都在下面）。

[CmdletBinding()]
param(
    [string[]]$Abi = @("arm64-v8a"),
    [int]$ApiLevel = 24,
    [switch]$DebugBuild,
    [switch]$DryRun,
    [switch]$Clean
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$profile = if ($DebugBuild) { "debug" } else { "release" }
$profileFlag = if ($DebugBuild) { "" } else { "--release" }

function Step([string]$text) { Write-Host "`n=== $text ===" -ForegroundColor Cyan }
function Fail([string]$text) { throw $text }

# ABI → (rust target, NDK clang 前缀, jniLibs 目录名)
$abiMap = @{
    "arm64-v8a"   = @{ Target = "aarch64-linux-android";        Clang = "aarch64-linux-android";        Env = "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER" }
    "armeabi-v7a" = @{ Target = "armv7-linux-androideabi";      Clang = "armv7a-linux-androideabi";     Env = "CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER" }
    "x86_64"      = @{ Target = "x86_64-linux-android";         Clang = "x86_64-linux-android";         Env = "CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER" }
    "x86"         = @{ Target = "i686-linux-android";           Clang = "i686-linux-android";           Env = "CARGO_TARGET_I686_LINUX_ANDROID_LINKER" }
}

# 允许 -Abi arm64-v8a,x86_64（从命令行传数组时会被当成一个字符串）
$Abi = $Abi | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ }
$unknown = $Abi | Where-Object { -not $abiMap.ContainsKey($_) }
if ($unknown) { Fail "不认识的 ABI：$($unknown -join ', ')（支持：$($abiMap.Keys -join ', ')）" }

# ---- 1. 找 NDK ----
Step "定位 Android NDK"
$ndkRoot = $env:ANDROID_NDK_HOME
if (-not $ndkRoot) {
    $sdk = if ($env:ANDROID_NDK_ROOT) { $null } elseif ($env:ANDROID_HOME) { $env:ANDROID_HOME } elseif ($env:ANDROID_SDK_ROOT) { $env:ANDROID_SDK_ROOT } else { $null }
    if ($sdk -and (Test-Path (Join-Path $sdk "ndk"))) {
        $cand = Get-ChildItem (Join-Path $sdk "ndk") -Directory | Sort-Object Name -Descending | Select-Object -First 1
        if ($cand) { $ndkRoot = $cand.FullName }
    }
}
if (-not $ndkRoot) {
    Fail @"
找不到 NDK。
  设置 ANDROID_NDK_HOME 指向 NDK 目录（例如 D:\dev\android-sdk\ndk\28.2.13676358），
  或确保 ANDROID_HOME 下有 ndk\<版本>。
"@
}
$hostTag = if ($IsWindows -or $env:OS -eq "Windows_NT") { "windows-x86_64" }
           elseif ($IsMacOS) { "darwin-x86_64" } else { "linux-x86_64" }
$binDir = Join-Path $ndkRoot "toolchains/llvm/prebuilt/$hostTag/bin"
if (-not (Test-Path $binDir)) { Fail "NDK 的 clang 目录不存在：$binDir" }
Write-Host "NDK：$ndkRoot"

# ---- 2. 检查 rust target ----
Step "检查 Rust Android target"
$installed = rustup target list --installed
$missing = @()
foreach ($a in $Abi) {
    $t = $abiMap[$a].Target
    if ($installed -notcontains $t) { $missing += $t }
}
if ($missing) { Fail "缺少 Rust target：$($missing -join ', ')`n请先运行： rustup target add $($missing -join ' ')" }

# ---- 3. 逐 ABI 交叉编译 ----
Push-Location $repo
try {
    if ($Clean) {
        Step "清理旧产物"
        foreach ($a in $Abi) {
            $jni = Join-Path $repo "app/android/app/src/main/jniLibs/$a/libyunong_core.so"
            if (Test-Path $jni) { Remove-Item $jni -Force; Write-Host "已删除 $jni" }
        }
    }

    $results = @()
    $script:ccVars = @()
    $script:arVars = @()
    foreach ($a in $Abi) {
        $info = $abiMap[$a]
        $clangName = "$($info.Clang)$ApiLevel-clang"
        $clang = Join-Path $binDir "$clangName.cmd"
        if (-not (Test-Path $clang)) {
            # 部分 NDK 版本没有 .cmd 包装（非 Windows 宿主）
            $clang = Join-Path $binDir $clangName
        }
        if (-not (Test-Path $clang)) {
            Fail "找不到 NDK 链接器：$clangName（API $ApiLevel）。可用 API 见 $binDir\*-clang*"
        }

        Step "交叉编译 $a（$($info.Target) / API $ApiLevel / $profile）"
        Write-Host "链接器：$clang"
        if ($DryRun) {
            Write-Host "[dry-run] `$env:$($info.Env) = '$clang'"
            Write-Host "[dry-run] cargo build -p yunong-core --target $($info.Target) $profileFlag"
        } else {
            # Rust 链接器
            Set-Item -Path "Env:$($info.Env)" -Value $clang
            # C 依赖（rusqlite bundled 的 sqlite3-src）用 NDK clang 编：cc crate 认 CC_<target> 形式
            $ccVar = "CC_" + ($info.Target -replace '-', '_')
            $arVar = "AR_" + ($info.Target -replace '-', '_')
            Set-Item -Path "Env:$ccVar" -Value $clang
            $llvmAr = Join-Path $binDir "llvm-ar.exe"
            if (Test-Path $llvmAr) { Set-Item -Path "Env:$arVar" -Value $llvmAr }
            $script:ccVars += $ccVar
            $script:arVars += $arVar
            cargo build -p yunong-core --target $($info.Target) $profileFlag
            if ($LASTEXITCODE -ne 0) { Fail "cargo build 失败（$a）：见上方 rustc 输出" }

            $soSrc = Join-Path $repo "target/$($info.Target)/$profile/libyunong_core.so"
            if (-not (Test-Path $soSrc)) { Fail "没找到产物：$soSrc" }
            $jniDir = Join-Path $repo "app/android/app/src/main/jniLibs/$a"
            New-Item -ItemType Directory -Force -Path $jniDir | Out-Null
            Copy-Item $soSrc (Join-Path $jniDir "libyunong_core.so") -Force
            $sizeMb = [math]::Round((Get-Item (Join-Path $jniDir "libyunong_core.so")).Length / 1MB, 2)
            Write-Host "OK $a → jniLibs/$a/libyunong_core.so ($sizeMb MB)"
            $results += [pscustomobject]@{ Abi = $a; Target = $($info.Target); SizeMB = $sizeMb; Path = (Join-Path $jniDir "libyunong_core.so") }
        }
    }

    if (-not $DryRun) {
        Step "产物汇总"
        $results | Format-Table -AutoSize | Out-String | Write-Host
        Write-Host "下一步：flutter build apk --release --target-platform android-arm64"
        Write-Host "（jniLibs 由 AGP 自动打进 APK；abiFilters 在 app/android/app/build.gradle.kts 里限制为 arm64-v8a）"
    }
} finally {
    Pop-Location
    # 清理本脚本设置的环境变量，避免污染同一会话的后续构建
    foreach ($a in $Abi) { Remove-Item "Env:$($abiMap[$a].Env)" -ErrorAction SilentlyContinue }
    foreach ($v in @($script:ccVars) + @($script:arVars)) {
        if ($v) { Remove-Item "Env:$v" -ErrorAction SilentlyContinue }
    }
}
