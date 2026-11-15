# 建通传的桌面 / 开始菜单快捷方式。
# 入口选托盘脚本而不是 tc.exe：`tc daemon` 是控制台前台程序，直接指过去会留一个黑窗口；
# 托盘脚本会隐藏启动 daemon、自带单实例保护，右键还能开出管理页。
param(
    [string]$AppDir = '',
    [string]$TcPath = '',
    [string]$Config = '',
    [switch]$NoStartMenu
)
$ErrorActionPreference = 'Stop'
if (-not $AppDir) { $AppDir = Split-Path -Parent (Split-Path -Parent $PSScriptRoot) }
if (-not $TcPath) { $TcPath = Join-Path $AppDir 'tc.exe' }
if (-not $Config) { $Config = Join-Path $AppDir 'config.toml' }

# 两种布局都要认：仓库里是 scripts\windows\tray.ps1，安装目录是 scripts\tray.ps1（安装包把 *.ps1 平铺进去）。
# 只认一种的话，安装版会在这里抛「缺少必需文件」，连带把快捷方式一起丢掉（2026-11-13 实测）。
$tray = Join-Path $AppDir 'scripts\windows\tray.ps1'
if (-not (Test-Path $tray)) { $tray = Join-Path $AppDir 'scripts\tray.ps1' }
# 图标只是锦上添花：安装版没有 assets 目录，缺了就用系统默认图标，不能因此中断
$ico = Join-Path $AppDir 'assets\tongchuan.ico'
if (-not (Test-Path $ico)) { $ico = Join-Path $AppDir 'tongchuan.ico' }
foreach ($p in @($TcPath, $Config, $tray)) {
    if (-not (Test-Path $p)) { throw "缺少必需文件：$p" }
}

# 入口改用 wscript + start-hidden.vbs：powershell.exe 是控制台程序，即使带 -WindowStyle Hidden
# 也会先建控制台窗口再隐藏，双击时能看到一闪（2026-11-15 实测）。vbs 用 Run(..., 0, False)
# 让窗口从创建起就是隐藏的，全程零可见窗口。
$vbs = Join-Path $AppDir 'scripts\windows\start-hidden.vbs'
if (-not (Test-Path $vbs)) { $vbs = Join-Path $AppDir 'scripts\start-hidden.vbs' }
if (-not (Test-Path $vbs)) { throw "缺少必需文件：$vbs" }
$wscript = Join-Path $env:SystemRoot 'System32\wscript.exe'
# -OpenUi：双击后除了保证后台在跑，还把管理页拉到浏览器（由 vbs 透传给 tray.ps1）
$psArgs = '//B //Nologo "{0}" -OpenUi' -f $vbs

function New-Lnk([string]$path) {
    $shell = New-Object -ComObject WScript.Shell
    $lnk = $shell.CreateShortcut($path)
    $lnk.TargetPath = $wscript
    $lnk.Arguments = $psArgs
    $lnk.WorkingDirectory = $AppDir
    if (Test-Path $ico) { $lnk.IconLocation = "$ico,0" }
    $lnk.Description = '通传 · 局域网文件夹互传（后台接收 + 托盘）'
    # 7 = 最小化：wscript 本身没有窗口，这一项只是兜底
    $lnk.WindowStyle = 7
    $lnk.Save()
    Write-Output "OK shortcut $path"
}

$targets = @(Join-Path ([Environment]::GetFolderPath('Desktop')) '通传.lnk')
if (-not $NoStartMenu) {
    $targets += Join-Path ([Environment]::GetFolderPath('Programs')) '通传.lnk'
}
foreach ($t in $targets) { New-Lnk $t }

Write-Output ("TARGET {0}" -f $ps)
Write-Output ("ARGS   {0}" -f $psArgs)
