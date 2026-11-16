# 手机截屏自动进 Windows 剪贴板。
# 手机端把截图通过现有文件通道发到本机 received 目录；这里监听新出现的 Screenshot* 图片，
# 直接用 System.Windows.Forms.Clipboard.SetImage 放进剪贴板，用户在电脑上 Ctrl+V 即可。
# 由 tray.ps1 以隐藏 -STA PowerShell 启动；带命名互斥量，重复启动会自动退出。
param(
    [string]$Inbox = (Join-Path $env:LOCALAPPDATA '通传\received'),
    [string]$LogFile = (Join-Path $env:LOCALAPPDATA '通传\logs\screenshot-clip.log')
)
$ErrorActionPreference = 'Continue'
$global:TCShotLog = $LogFile

$created = $false
$mutex = New-Object System.Threading.Mutex($true, 'Local\TongchuanScreenshotClip', [ref]$created)
if (-not $created) { exit 0 }

function Write-ShotLog([string]$line) {
    try {
        $dir = Split-Path -Parent $LogFile
        if ($dir -and -not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
        Add-Content -LiteralPath $LogFile -Value ((Get-Date -Format 'yyyy-MM-dd HH:mm:ss') + ' ' + $line) -Encoding UTF8
    } catch { }
}

if (-not (Test-Path -LiteralPath $Inbox)) {
    Write-ShotLog "ERROR inbox not found: $Inbox"
    exit 1
}

try {
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
} catch {
    Write-ShotLog "ERROR load clipboard assemblies: $($_.Exception.Message)"
    exit 1
}

$action = {
    $path = $Event.SourceEventArgs.FullPath
    if ($path -notmatch '(?i)(Screenshot[^\\/]*\.(png|jpe?g|webp))$') { return }
    Start-Sleep -Milliseconds 700
    if (-not (Test-Path -LiteralPath $path)) { return }
    try {
        $img = [System.Drawing.Image]::FromFile($path)
        [System.Windows.Forms.Clipboard]::SetImage($img)
        $img.Dispose()
        Add-Content -LiteralPath $global:TCShotLog -Encoding UTF8 -Value ((Get-Date -Format 'yyyy-MM-dd HH:mm:ss') + " SET_IMAGE $path")
    } catch {
        Add-Content -LiteralPath $global:TCShotLog -Encoding UTF8 -Value ((Get-Date -Format 'yyyy-MM-dd HH:mm:ss') + " ERROR $path : $($_.Exception.Message)")
    }
}

$watcher = New-Object System.IO.FileSystemWatcher $Inbox, '*.*'
$watcher.IncludeSubdirectories = $true
$watcher.NotifyFilter = [System.IO.NotifyFilters]'FileName, LastWrite, Size'
$null = Register-ObjectEvent -InputObject $watcher -EventName Created -Action $action
$null = Register-ObjectEvent -InputObject $watcher -EventName Renamed -Action $action
$watcher.EnableRaisingEvents = $true
Write-ShotLog "watch $Inbox"

while ($true) { Start-Sleep -Seconds 1 }
