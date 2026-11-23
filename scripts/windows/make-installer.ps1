# 编译 NSIS 安装包：版本号与 tc.exe 路径都从项目自己的配置派生，避免再手写一份。
# 版本号唯一来源是根 Cargo.toml；构建产物目录优先取 CARGO_TARGET_DIR，否则用项目上一级的
# tongchuan-target（ring 的 ar 处理不了中文路径，所以不放项目内）。
# 安装包不落在仓库里：默认输出到仓库上一级的 dist\通传\release-v<版本>\。
# 用法：& pwsh -NoProfile -File scripts/windows/make-installer.ps1 [-DryRun] [-OutDir <目录>]
# -OutDir 仅供验证时把安装包打到 cache\ 里、避免覆盖已发布的安装包；不传则走上面的默认路径。
param([switch]$DryRun, [string]$OutDir)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

# 版本号：Windows 端有独立发版号时（用户 2026-11-23 定 Windows 端 0.21.0），
# scripts/windows/VERSION 优先；没有该文件时回落到根 Cargo.toml（与安卓/整体一致）。
$verFile = Join-Path $PSScriptRoot 'VERSION'
if (Test-Path -LiteralPath $verFile) {
    $version = (Get-Content -LiteralPath $verFile -Raw).Trim()
    if ($version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') {
        Write-Output 'FAIL: scripts/windows/VERSION 不是 X.Y.Z'
        exit 1
    }
} else {
    $toml = Get-Content (Join-Path $root 'Cargo.toml') -Raw
    if ($toml -notmatch '(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"') {
        Write-Output 'FAIL: 从根 Cargo.toml 读不到版本号'
        exit 1
    }
    $version = $Matches[1]
}

# 发版前门禁：打安装包前跑跨项目规范检查，FAIL 中止、WARN 只打印。
# 为什么放在读出 $version 之后：C1.5 要按版本号只校验本次要发的包，历史目录归存量。
# 为什么检查器缺失只警告：构建机不一定装了 pwsh 7，硬阻断会让该环境无法出包。
$specCheck = Join-Path (Split-Path -Parent $root) 'tools\spec-check\check-spec.ps1'
if ((Get-Command pwsh -ErrorAction SilentlyContinue) -and (Test-Path -LiteralPath $specCheck)) {
    & pwsh -NoProfile -File $specCheck -Project $root -Name '通传' -Target $version -IssuesOnly
    if ($LASTEXITCODE -ne 0) { throw '规范检查未通过（见上方 FAIL 项），本次发版中止' }
}
else {
    Write-Warning '未找到 pwsh 或规范检查脚本，跳过发版前检查'
}

# 安装包输出目录：仓库上一级的 dist\通传\release-v<版本>，按版本分目录；
# 传了 -OutDir 就改用指定目录（验证时指向 cache\，不动已发布的安装包）
if (-not $OutDir) {
    $OutDir = Join-Path (Join-Path (Join-Path (Split-Path -Parent $root) 'dist') '通传') ("release-v{0}" -f $version)
} elseif (-not [System.IO.Path]::IsPathRooted($OutDir)) {
    # 相对路径按项目根解析并转绝对路径：makensis 会先切到 scripts\windows 再跑，相对路径会落到错位置
    $OutDir = Join-Path $root $OutDir
}
$outDir = [System.IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

# 构建缓存统一放在仓库外，与 .cargo/config.toml 的 target-dir 保持一致
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path (Split-Path -Parent $root) 'tongchuan-target' }
$tcPath = Join-Path $targetDir 'release\tc.exe'
if (-not (Test-Path $tcPath)) {
    Write-Output "FAIL: 没找到 tc.exe：$tcPath（先跑 scripts/verify.ps1 构建）"
    exit 1
}

# makensis 通常不在 PATH：按 PATH → 项目上一级 tools\nsis → NSIS 默认安装位置 依次找。
$toolRoot = Join-Path (Split-Path -Parent $root) 'tools'
$makensisChecks = [ordered]@{
    'PATH 中的 makensis'      = (Get-Command makensis -ErrorAction SilentlyContinue).Source
    '项目上一级\tools\nsis'    = (Join-Path $toolRoot 'nsis\makensis.exe')
}
# Program Files 变量在个别环境下为空，Join-Path 会直接抛异常，所以先判空再拼
if ($env:ProgramFiles) { $makensisChecks['Program Files\NSIS'] = Join-Path $env:ProgramFiles 'NSIS\makensis.exe' }
if (${env:ProgramFiles(x86)}) { $makensisChecks['Program Files (x86)\NSIS'] = Join-Path ${env:ProgramFiles(x86)} 'NSIS\makensis.exe' }

$makensis = $null
foreach ($label in $makensisChecks.Keys) {
    $p = $makensisChecks[$label]
    if ($p -and (Test-Path $p)) { $makensis = $p; break }
}

$nsiArgs = @("/DVERSION=$version", "/DTC_BUILD=$tcPath", "/DOUT_DIR=$outDir", 'installer.nsi')
if ($DryRun) {
    $exe = if ($makensis) { $makensis } else { 'makensis.exe（未找到，仅演示参数）' }
    Write-Output ("DRY-RUN {0} {1}" -f $exe, ($nsiArgs -join ' '))
    exit 0
}
if (-not $makensis) {
    Write-Output 'FAIL: 找不到 makensis，已依次检查：'
    foreach ($label in $makensisChecks.Keys) { Write-Output ("  - {0}: {1}" -f $label, $makensisChecks[$label]) }
    Write-Output '装 NSIS 后再跑；也可加 -DryRun 只看参数'
    exit 1
}

Push-Location $PSScriptRoot
& $makensis @nsiArgs
$code = $LASTEXITCODE
Pop-Location
if ($code -ne 0) {
    Write-Output "FAIL: makensis 失败（exit $code）"
    exit $code
}

# 规范 5.6 要求每个交付件旁配同名 .sha256；装完立刻生成，让「安装包已出、校验缺失」
# 这种半成品在脚本最后一步就暴露，而不是等发版门禁才发现。
function Write-Sha256File([string]$Path) {
    $hash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLower()
    $sumPath = "$Path.sha256"
    # UTF-8 无 BOM + LF：内容按「小写哈希 + 两个空格 + 文件名」，与 sha256sum -c 的格式一致。
    # 不能用 ASCII——安装包名含「通传」两个汉字，ASCII 会把它们写成 ??，校验时找不到文件
    [System.IO.File]::WriteAllText($sumPath, "$hash  $([System.IO.Path]::GetFileName($Path))`n",
        (New-Object System.Text.UTF8Encoding($false)))
    if (-not (Test-Path -LiteralPath $sumPath) -or (Get-Item -LiteralPath $sumPath).Length -eq 0) {
        Write-Output "FAIL: 校验文件未写出：$sumPath"
        exit 1
    }
    return $sumPath
}

# 安装包名由 installer.nsi 的 OutFile 决定：${OUT_DIR}\通传-${VERSION}-setup.exe
$installer = Join-Path $outDir ("通传-{0}-setup.exe" -f $version)
if (-not (Test-Path -LiteralPath $installer)) {
    Write-Output "FAIL: makensis 成功但没找到安装包：$installer"
    exit 1
}
$sumPath = Write-Sha256File $installer

Write-Output ("PASS: 通传-{0}-setup.exe -> {1}" -f $version, $outDir)
Write-Output ("PASS: 校验 -> {0}" -f $sumPath)
