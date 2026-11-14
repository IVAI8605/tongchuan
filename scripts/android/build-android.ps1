# 交叉编译 tc-jni 到 Android，并把 .so 放进 Android 工程的 jniLibs。
#
# 为什么必须显式给 CC/AR/CXX：ring 的 C 代码走 cc-rs 编译，而 cc-rs 不会自己从
# NDK 目录推出版本号化的 clang 名字（它找的是没有版本号的 aarch64-linux-android-clang），
# 只设 ANDROID_NDK_HOME 会直接报 "failed to find tool"，这一点已实测。
#
# 用法：& pwsh -NoProfile -File scripts/android/build-android.ps1 [-Abi arm64-v8a]

param(
    [ValidateSet("arm64-v8a", "armeabi-v7a", "x86_64")]
    [string]$Abi = "arm64-v8a",
    [string]$Api = "24"
)

$ErrorActionPreference = "Continue"

# 用 -NoProfile 跑时不会加载用户 profile，cargo 可能不在 PATH 里；这里兜一层，脚本自己保证能找到工具。
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    if (Test-Path (Join-Path $cargoBin "cargo.exe")) {
        $env:PATH = "$cargoBin;$env:PATH"
    } else {
        Write-Host "FAIL: 找不到 cargo，$cargoBin 下也没有" -ForegroundColor Red
        exit 1
    }
}

$projectRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$toolRoot = Join-Path (Split-Path -Parent $projectRoot) 'tools'

# 工具链查找：SDK 可能装在项目上一级的 tools\Base\Android，但系统环境变量可能还留着旧值。
# 所以不能只看变量有没有值，必须验证路径真实存在；
# 顺序为 环境变量 → 本机默认安装位置 → tools 兜底，全都不中才报错并列出查过哪些位置。
$sdkChecks = [ordered]@{
    'ANDROID_HOME'                    = $env:ANDROID_HOME
    'ANDROID_SDK_ROOT'                = $env:ANDROID_SDK_ROOT
    '%LOCALAPPDATA%\Android\Sdk'      = (Join-Path $env:LOCALAPPDATA 'Android\Sdk')
    '项目上一级\tools\Base\Android\Sdk' = (Join-Path $toolRoot 'Base\Android\Sdk')
}
$sdk = $null
foreach ($label in $sdkChecks.Keys) {
    if ($sdkChecks[$label] -and (Test-Path $sdkChecks[$label])) { $sdk = $sdkChecks[$label]; break }
}
if (-not $sdk) {
    Write-Host 'FAIL: 找不到 Android SDK，已依次检查：' -ForegroundColor Red
    foreach ($label in $sdkChecks.Keys) {
        $p = $sdkChecks[$label]
        $state = if (-not $p) { '（未设置）' } elseif (Test-Path $p) { '存在' } else { '不存在' }
        Write-Host ("  - {0} = {1} {2}" -f $label, $p, $state)
    }
    exit 1
}

$ndkRoot = Join-Path $sdk 'ndk'
$ndkChecks = [ordered]@{
    'ANDROID_NDK_HOME' = $env:ANDROID_NDK_HOME
    'ANDROID_NDK_ROOT' = $env:ANDROID_NDK_ROOT
}
$ndk = $null
foreach ($label in $ndkChecks.Keys) {
    if ($ndkChecks[$label] -and (Test-Path $ndkChecks[$label])) { $ndk = $ndkChecks[$label]; break }
}
if (-not $ndk) {
    # 环境变量都没命中时扫 SDK 下的 ndk 目录，装了多份就取版本号最大的（目录名形如 27.3.13750724）
    $ndk = Get-ChildItem $ndkRoot -Directory -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending | Select-Object -First 1 -ExpandProperty FullName
}
if (-not $ndk) {
    $ndkRootState = if (Test-Path $ndkRoot) { '目录存在但没有可用版本' } else { '目录不存在' }
    Write-Host 'FAIL: 找不到 Android NDK，已依次检查：' -ForegroundColor Red
    foreach ($label in $ndkChecks.Keys) {
        $p = $ndkChecks[$label]
        $state = if (-not $p) { '（未设置）' } elseif (Test-Path $p) { '存在' } else { '不存在' }
        Write-Host ("  - {0} = {1} {2}" -f $label, $p, $state)
    }
    Write-Host ("  - SDK 内 ndk 目录 {0}：{1}" -f $ndkRoot, $ndkRootState)
    exit 1
}
Write-Host "SDK $sdk / NDK $ndk"
$ndkBin = Join-Path $ndk "toolchains\llvm\prebuilt\windows-x86_64\bin"

if (-not (Test-Path $ndkBin)) {
    Write-Host "FAIL: NDK 工具链不存在：$ndkBin" -ForegroundColor Red
    exit 1
}

# rust 目标三元组 / NDK 里的 clang 前缀并不总相同（armv7 的 rust 目标是 armv7-linux-androideabi，
# 而 clang 前缀是 armv7a-linux-androideabi），所以分开写，不能用同一个字符串拼。
$map = @{
    "arm64-v8a"   = @{ rust = "aarch64-linux-android"; clang = "aarch64-linux-android" }
    "armeabi-v7a" = @{ rust = "armv7-linux-androideabi"; clang = "armv7a-linux-androideabi" }
    "x86_64"      = @{ rust = "x86_64-linux-android";   clang = "x86_64-linux-android" }
}
$t = $map[$Abi]
$envKey = $t.rust -replace '-', '_'

$env:ANDROID_HOME = $sdk
$env:ANDROID_SDK_ROOT = $sdk
$env:ANDROID_NDK_HOME = $ndk
$env:ANDROID_NDK_ROOT = $ndk
Set-Item -Path "Env:CC_$envKey"  -Value (Join-Path $ndkBin "$($t.clang)$Api-clang.cmd")
Set-Item -Path "Env:CXX_$envKey" -Value (Join-Path $ndkBin "$($t.clang)$Api-clang++.cmd")
Set-Item -Path "Env:AR_$envKey"  -Value (Join-Path $ndkBin "llvm-ar.exe")
# 链接器走 cargo 的环境变量映射，替代原先写死在 .cargo/config.toml 里的 NDK 绝对路径
Set-Item -Path "Env:CARGO_TARGET_$(($t.rust -replace '-', '_').ToUpper())_LINKER" -Value (Join-Path $ndkBin "$($t.clang)$Api-clang.cmd")

# 产物目录：环境变量优先，否则与 .cargo/config.toml 的 target-dir 同一相对位置（项目上一级/tongchuan-target）
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path (Split-Path -Parent $projectRoot) 'tongchuan-target' }
$env:CARGO_TARGET_DIR = $targetDir

Write-Host "编译 $Abi ($($t.rust), API $Api) ..."
Push-Location $projectRoot
& cargo build --target $t.rust -p tc-jni --release
$code = $LASTEXITCODE
Pop-Location
if ($code -ne 0) {
    Write-Host "FAIL: cargo build 失败（exit $code）" -ForegroundColor Red
    exit $code
}

$so = Join-Path $targetDir "$($t.rust)\release\libtc_jni.so"
if (-not (Test-Path $so)) {
    Write-Host "FAIL: 没找到产物 $so" -ForegroundColor Red
    exit 1
}

$jniLibs = Join-Path $projectRoot "android\app\src\main\jniLibs\$Abi"
New-Item -ItemType Directory -Force -Path $jniLibs | Out-Null
Copy-Item $so (Join-Path $jniLibs "libtc_jni.so") -Force

$size = [math]::Round((Get-Item $so).Length / 1KB, 1)
Write-Host "PASS: $Abi libtc_jni.so ($size KB) -> android\app\src\main\jniLibs\$Abi" -ForegroundColor Green
