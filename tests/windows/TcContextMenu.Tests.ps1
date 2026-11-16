# 右键发送入口的纯参数测试（不依赖 Windows 注册表，可在 NAS 的 pwsh 上跑）。
# 运行：pwsh -NoProfile -File tests/windows/TcContextMenu.Tests.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
. (Join-Path $root 'scripts/windows/TcContextMenu.ps1')

function Assert-ArrayEqual([string]$name, [string[]]$actual, [string[]]$expected) {
    if (($actual -join "`n") -ne ($expected -join "`n")) {
        throw ("{0} 失败`nexpected=[{1}]`nactual=[{2}]" -f $name, ($expected -join ' | '), ($actual -join ' | '))
    }
    Write-Output "PASS $name"
}

# 多路径、带空格路径、带指纹和 dest_id
$actual = @(New-TcSendArguments -TcPath 'C:\通传\tc.exe' -Target '192.168.95.16:53318' `
    -PeerFp 'abc123' -DestId 'phone' -Paths @('C:\A B\one.txt', 'D:\folder'))
$expected = @(
    'send', '--to', '192.168.95.16:53318',
    '--peer-fp', 'abc123', '--dest-id', 'phone',
    '--dir', 'C:\A B\one.txt', '--dir', 'D:\folder'
)
Assert-ArrayEqual '多路径参数' $actual $expected

# 不带可选参数
$actual = @(New-TcSendArguments -TcPath 'tc.exe' -Target 'host:53318' -Paths @('x'))
$expected = @('send', '--to', 'host:53318', '--dir', 'x')
Assert-ArrayEqual '最小参数' $actual $expected

# 空路径必须拒绝
try {
    New-TcSendArguments -TcPath 'tc.exe' -Target 'host:53318' -Paths @()
    throw '空路径未被拒绝'
} catch {
    if ($_.Exception.Message -notmatch '至少一个路径') { throw }
    Write-Output 'PASS 空路径拒绝'
}

# SendTo 链接名只保留可显示字符
$safe = Get-TcSafeLinkName '手机: V2551A / 收件'
if ($safe -match '[\\/:*?"<>|]') { throw "非法链接名仍含保留字符：$safe" }
Write-Output "PASS 链接名 $safe"

Write-Output 'TcContextMenu.Tests: ALL PASS'
exit 0
