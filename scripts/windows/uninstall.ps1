# 通传 v0.3 卸载。默认 dry-run：只列清单，不改任何东西。
# 只有显式传 -Confirm 才真正清理；清理范围仅限本脚本创建的那几个具体注册表项。
# 用户文件（配置、日志、历史、收到的文件）一律不动，需要清理时另列清单询问用户。
param([switch]$Confirm)
$ErrorActionPreference = 'Continue'

$keys = @(
    'HKCU:\Software\Classes\Directory\shell\TongchuanSend',
    'HKCU:\Software\Classes\*\shell\TongchuanSend',
    'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
)
Write-Output '=== 将清理的注册表范围（只列清单）==='
Write-Output 'HKCU:\Software\Classes\Directory\shell\TongchuanSend（整键）'
Write-Output 'HKCU:\Software\Classes\*\shell\TongchuanSend（整键）'
Write-Output 'HKCU:\...\Run 下的 Tongchuan 值'
Write-Output '防火墙规则：通传 TCP 53318 / 通传 UDP 53318'
Write-Output '不删除：配置文件、日志、历史、收到的文件、身份证书'

if (-not $Confirm) {
    Write-Output 'DRY-RUN 未做任何改动。确认后加 -Confirm 再运行。'
    exit 0
}

Remove-Item -Path $keys[0] -Recurse -ErrorAction SilentlyContinue
Remove-Item -Path $keys[1] -Recurse -ErrorAction SilentlyContinue
Remove-ItemProperty -Path $keys[2] -Name 'Tongchuan' -ErrorAction SilentlyContinue
foreach ($proto in @('TCP', 'UDP')) {
    & netsh advfirewall firewall delete rule name="通传 $proto 53318" *> $null
}
Write-Output 'OK 已清理右键菜单、登录启动与防火墙规则；用户数据未动'
