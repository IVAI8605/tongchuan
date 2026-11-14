# 通传 · Windows 剪贴板桥（v0.6 内核配套）
#
# 作用：本机剪贴板一变，就把新内容推到对端的剪贴板中继。
# 反方向（对端 → 本机）由 `tc clip watch --apply` 负责：它收到事件后调
# PowerShell 的 Set-Clipboard 写回来。两边合起来就是「双向自动」。
#
# 用法：
#   .\clipboard-bridge.ps1 -TcPath .\tc.exe -Server <对端IP:53320> -PeerFp <对端指纹> -Identity .\identity
# 说明：
#   - 只同步文本；图片/文件走文件传输通道，不塞进剪贴板。
#   - 单实例运行：重复启动会直接退出，避免同一条内容被推两遍。
#   - 对端按内容哈希去重，即使本机把「收到的内容」又推回去，也不会形成回环。

param(
    [string]$TcPath = ".\tc.exe",
    [string]$Server = "",
    [string]$PeerFp = "",
    [string]$Identity = "",
    [string]$Origin = $env:COMPUTERNAME,
    [int]$IntervalMs = 700
)

$ErrorActionPreference = 'Continue'

# 服务器地址不再给默认值：写死某个内网地址会在换环境时静默连错对端，改为强制显式传参
if (-not $Server) {
    Write-Output "CLIP-BRIDGE needs -Server <IP:端口>"
    exit 1
}

# 单实例：剪贴板桥开两个实例没有意义，只会互相把同一条内容推两次
$mutex = New-Object System.Threading.Mutex($false, "Global\TongchuanClipBridge")
if (-not $mutex.WaitOne(0)) {
    Write-Output "CLIP-BRIDGE already running"
    exit 1
}

function Get-ClipHash([string]$text) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($text)
    return [System.BitConverter]::ToString($sha.ComputeHash($bytes))
}

# 先读一次当前剪贴板作为基线：直接进循环会把启动前就存在的旧内容也推上去
$last = ""
try {
    $cur = Get-Clipboard -Raw -ErrorAction Stop
    if ($cur) { $last = Get-ClipHash $cur }
} catch { }

Write-Output "CLIP-BRIDGE start server=$Server origin=$Origin"

while ($true) {
    try {
        $text = Get-Clipboard -Raw -ErrorAction Stop
    } catch {
        $text = $null
    }
    if ($text) {
        $h = Get-ClipHash $text
        if ($h -ne $last) {
            $last = $h
            $argv = @("clip", "push", "--to", $Server, "--stdin", "--origin", $Origin)
            if ($PeerFp) { $argv += @("--peer-fp", $PeerFp) }
            if ($Identity) { $argv += @("--identity", $Identity) }
            $out = $text | & $TcPath @argv 2>&1
            Write-Output ("CLIP-BRIDGE push " + ($out | Out-String).Trim())
        }
    }
    Start-Sleep -Milliseconds $IntervalMs
}
