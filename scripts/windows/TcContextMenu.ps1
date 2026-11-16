# 通传 Windows 右键发送的纯参数构造。
# 只做参数拼装，不碰注册表、不启动进程，便于在 NAS 的 pwsh 上直接测试。

function New-TcSendArguments {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$TcPath,
        [Parameter(Mandatory = $true)][string]$Target,
        [string]$PeerFp = '',
        [string]$DestId = '',
        [string[]]$Paths = @()
    )

    if (-not $TcPath) { throw '缺少 tc.exe 路径' }
    if (-not $Target) { throw '缺少目标地址' }
    $cleanPaths = @($Paths | Where-Object { $_ -and ([string]$_).Trim() })
    if ($cleanPaths.Count -eq 0) { throw '右键发送至少一个路径' }

    $args = @('send', '--to', $Target)
    if ($PeerFp) { $args += @('--peer-fp', $PeerFp) }
    if ($DestId) { $args += @('--dest-id', $DestId) }
    foreach ($p in $cleanPaths) { $args += @('--dir', [string]$p) }
    return $args
}

function Get-TcSafeLinkName {
    param([string]$Name)
    if (-not $Name) { return '设备' }
    # Windows 文件名不能包含 \ / : * ? " < > |
    $safe = $Name -replace '[\\/:*?"<>|]', '_'
    $safe = $safe -replace '\s+', ' '
    $safe = $safe.Trim()
    if (-not $safe) { return '设备' }
    return $safe
}
