# 一键出 Android APK：先交叉编译 Rust 内核，再交给 Gradle 打包。
#
# 为什么用缓存里的 Gradle 而不是 gradlew：项目没带 wrapper jar，而本机已经有一份
# 8.11.1 发行版，直接调它最省事，也不用联网再下一份。
#
# 用法：& pwsh -NoProfile -File scripts/android/build-apk.ps1

param(
    [switch]$Online
)

$ErrorActionPreference = "Continue"
$here = $PSScriptRoot
$projectRoot = Split-Path -Parent (Split-Path -Parent $here)

# 发版前门禁：出包前跑跨项目规范检查，FAIL 中止、WARN 只打印。
# 为什么放在这里：Rust 交叉编译与 Gradle 打包都耗时，规范问题应在动手前暴露。
# 为什么不传 -Target：此刻 APK 还没生成，带目标会被判「本次发版包缺同名 .sha256」而必然
# FAIL（实测 2026-11-13 20:20）。打包完成后本脚本自己写 .sha256 并校验，见文件末尾。
# 为什么检查器缺失只警告：非本机环境可能没有 pwsh 7，硬阻断会让该环境无法出包。
$specCheck = Join-Path (Split-Path -Parent $projectRoot) 'tools\spec-check\check-spec.ps1'
if ((Get-Command pwsh -ErrorAction SilentlyContinue) -and (Test-Path -LiteralPath $specCheck)) {
    & pwsh -NoProfile -File $specCheck -Project $projectRoot -Name '通传' -IssuesOnly
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAIL: 规范检查未通过（见上方 FAIL 项），本次发版中止" -ForegroundColor Red
        exit 1
    }
}
else {
    Write-Warning '未找到 pwsh 或规范检查脚本，跳过发版前检查'
}

# 不写死本机路径：JAVA_HOME 未设时依次看 PATH 里的 java 与用户目录下的 jdk-*
if (-not $env:JAVA_HOME) {
    $java = (Get-Command java -ErrorAction SilentlyContinue).Source
    if ($java) {
        $env:JAVA_HOME = Split-Path -Parent (Split-Path -Parent $java)
    } else {
        $jdk = Get-ChildItem (Join-Path $env:USERPROFILE '.local') -Directory -Filter 'jdk-*' -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending | Select-Object -First 1
        if ($jdk) { $env:JAVA_HOME = $jdk.FullName }
    }
}
$sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } elseif ($env:ANDROID_SDK_ROOT) { $env:ANDROID_SDK_ROOT } else { Join-Path $env:LOCALAPPDATA 'Android\Sdk' }
$env:ANDROID_HOME = $sdk
$env:ANDROID_SDK_ROOT = $sdk

if (-not $env:JAVA_HOME -or -not (Test-Path (Join-Path $env:JAVA_HOME "bin\java.exe"))) {
    Write-Host "FAIL: 找不到 JDK：$env:JAVA_HOME（请设置 JAVA_HOME）" -ForegroundColor Red
    exit 1
}

# 第一步：Rust 内核
& pwsh -NoProfile -File (Join-Path $here "build-android.ps1")
if ($LASTEXITCODE -ne 0) {
    Write-Host "FAIL: Rust 交叉编译失败" -ForegroundColor Red
    exit 1
}

# 第二步：找 Gradle 发行版
$gradle = Join-Path $env:USERPROFILE ".gradle\wrapper\dists"
$gradleBat = Get-ChildItem $gradle -Recurse -Filter "gradle.bat" -ErrorAction SilentlyContinue |
    Select-Object -First 1 -ExpandProperty FullName
if (-not $gradleBat) {
    Write-Host "FAIL: 本机没有可用的 Gradle 发行版（$gradle 下没有 gradle.bat）" -ForegroundColor Red
    exit 1
}

# 默认离线：所有依赖都已在本机 Gradle 缓存里，走网络只会变慢甚至卡住
$gradleArgs = @("assembleRelease")
if (-not $Online) { $gradleArgs += "--offline" }
$gradleArgs += "--console=plain"

Push-Location (Join-Path $projectRoot "android")
& $gradleBat @gradleArgs
$code = $LASTEXITCODE
Pop-Location
if ($code -ne 0) {
    Write-Host "FAIL: Gradle 打包失败（exit $code）" -ForegroundColor Red
    exit $code
}

$apk = Join-Path $projectRoot "android\app\build\outputs\apk\release\app-release.apk"
if (-not (Test-Path $apk)) {
    Write-Host "FAIL: 构建成功但没找到 APK" -ForegroundColor Red
    exit 1
}

# 规范 5.6：交付件旁要配同名 .sha256。APK 一落地就写，缺校验即判本次打包失败，
# 避免「有包没校验」的产物被直接拿去做真机验证与归档。
function Write-Sha256File([string]$Path) {
    $hash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLower()
    $sumPath = "$Path.sha256"
    # 内容按「小写哈希 + 两个空格 + 文件名」，UTF-8 无 BOM、LF 换行，与 sha256sum -c 兼容
    [System.IO.File]::WriteAllText($sumPath, "$hash  $([System.IO.Path]::GetFileName($Path))`n",
        (New-Object System.Text.UTF8Encoding($false)))
    if (-not (Test-Path -LiteralPath $sumPath) -or (Get-Item -LiteralPath $sumPath).Length -eq 0) {
        Write-Host "FAIL: 校验文件未写出：$sumPath" -ForegroundColor Red
        exit 1
    }
    return $sumPath
}
$sumPath = Write-Sha256File $apk

$mb = [math]::Round((Get-Item $apk).Length / 1MB, 2)
Write-Host "PASS: APK $apk ($mb MB)" -ForegroundColor Green
Write-Host "PASS: 校验 -> $sumPath" -ForegroundColor Green
