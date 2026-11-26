# 通传 Windows 桌面壳一键构建：出 tc.exe -> 自动放进 Tauri resources -> 校验版本 -> 出 NSIS 安装包。
# 取代 docs/README 里手工 copy 的步骤，避免 tc.exe 与 Cargo.toml 版本漂移。
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $root 'Cargo.toml'
$tauriDir = Join-Path $root 'windows-gui/src-tauri'
$binDir = Join-Path $tauriDir 'bin'
$tcExe = Join-Path $binDir 'tc.exe'

# 版本唯一来源：根 Cargo.toml 的 workspace.package.version
$verLine = Select-String -Path $manifest -Pattern '^version = "([^"]+)"' | Select-Object -First 1
if (-not $verLine) { throw "Cargo.toml 里找不到 version" }
$ver = $verLine.Matches[0].Groups[1].Value

# 1) 出 tc.exe（Windows 目标；CARGO_TARGET_DIR 优先）
$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) { $cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
Push-Location $root
try {
    & $cargo build --release -p tongchuan
    if ($LASTEXITCODE -ne 0) { throw "cargo build 失败（exit=$LASTEXITCODE）" }
} finally { Pop-Location }
$built = Join-Path $root 'target/release/tc.exe'
if ($env:CARGO_TARGET_DIR) { $built = Join-Path $env:CARGO_TARGET_DIR 'release/tc.exe' }
if (-not (Test-Path $built)) { throw "找不到构建产物 $built" }

# 2) 自动 copy 到 Tauri resources 并校验版本（只覆盖 tc.exe，不删别的）
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
Copy-Item -Force $built $tcExe
$got = (& $tcExe version | Out-String).Trim()
if ($got -ne $ver) { throw "tc.exe 版本 $got 与 Cargo.toml $ver 不一致" }
Write-Output ("TC_EXE PASS version=" + $got)

# 3) 桌面壳自己的两处版本也要跟 Cargo 一致
foreach ($p in @((Join-Path $tauriDir 'Cargo.toml'), (Join-Path $tauriDir 'tauri.conf.json'))) {
    $txt = Get-Content $p -Raw
    if ($txt -notmatch [regex]::Escape($ver)) { throw "$p 里没有版本 $ver" }
}

# 4) 出 NSIS 安装包
Push-Location $tauriDir
try {
    & npx '@tauri-apps/cli@2' build
    if ($LASTEXITCODE -ne 0) { throw "tauri build 失败（exit=$LASTEXITCODE）" }
} finally { Pop-Location }
Write-Output "DESKTOP PASS"
