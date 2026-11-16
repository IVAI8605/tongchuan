# SendTo 入口的参数绑定回归：确保选中路径不会落到 -DestId 上。
# 运行：pwsh -NoProfile -File tests/windows/SendFromExplorer.Tests.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('tc-sendto-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$fake = Join-Path $tmp 'fake-tc.ps1'
Set-Content -LiteralPath $fake -Encoding utf8 -Value 'param([Parameter(ValueFromRemainingArguments=$true)][string[]]$Args); $Args | ForEach-Object { Write-Output $_ }'

$env:TC_EXE = $fake
$out = @(& pwsh -NoProfile -File (Join-Path $root 'scripts/windows/send-from-explorer.ps1') `
    -Target '192.168.95.16:53318' -PeerFp 'abc123' 'C:\A B\one.txt' 'D:\folder' 2>&1)
$joined = $out -join "`n"
if ($joined -notmatch '--to') { throw "缺少 --to：`n$joined" }
if ($joined -notmatch '--dir') { throw "缺少 --dir：`n$joined" }
if ($joined -notmatch [regex]::Escape('C:\A B\one.txt')) { throw "第一个路径未转发：`n$joined" }
if ($joined -notmatch [regex]::Escape('D:\folder')) { throw "第二个路径未转发：`n$joined" }
if ($joined -match '--dest-id[\r\n]+C:\\') { throw "路径仍被误当成 dest-id：`n$joined" }
Write-Output 'PASS SendTo 多路径绑定'
Write-Output 'SendFromExplorer.Tests: ALL PASS'
exit 0
