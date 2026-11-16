# Explorer「发送到」入口：接收目标与所有选中路径，调用 tc send。
# SendTo 快捷方式把选中项追加到本脚本参数末尾；Paths 用 ValueFromRemainingArguments 接住。
# 关闭位置参数绑定：否则未命名的前两个路径会被 DestId/Paths 吞掉（多选发送会错位）。
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory = $true)][string]$Target,
    [string]$PeerFp = '',
    [string]$DestId = '',
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$Paths
)
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'TcContextMenu.ps1')

# 仓库布局是 scripts/windows/，安装版是 scripts/；两种都按 tc.exe 所在目录识别应用根。
$appRoot = Split-Path -Parent $PSScriptRoot
if ((Split-Path -Leaf $appRoot) -eq 'windows') { $appRoot = Split-Path -Parent $appRoot }
$tcPath = if ($env:TC_EXE) { $env:TC_EXE } else { Join-Path $appRoot 'tc.exe' }
if (-not (Test-Path -LiteralPath $tcPath)) {
    throw "找不到 tc.exe：$tcPath"
}

$sendArgs = @(New-TcSendArguments -TcPath $tcPath -Target $Target -PeerFp $PeerFp -DestId $DestId -Paths $Paths)
& $tcPath @sendArgs
exit $LASTEXITCODE
