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
Write-Output '防火墙规则：通传 入站 / 通传 入站（公用，限本地子网）'
Write-Output '不删除：配置文件、日志、历史、收到的文件、身份证书'

if (-not $Confirm) {
    Write-Output 'DRY-RUN 未做任何改动。确认后加 -Confirm 再运行。'
    exit 0
}

Remove-Item -Path $keys[0] -Recurse -ErrorAction SilentlyContinue
Remove-Item -Path $keys[1] -Recurse -ErrorAction SilentlyContinue
foreach ($k in @(
    'HKCU:\Software\Classes\Directory\shell\TongchuanSendPhone',
    'HKCU:\Software\Classes\*\shell\TongchuanSendPhone',
    'HKCU:\Software\Classes\Directory\shell\TongchuanSendNAS',
    'HKCU:\Software\Classes\*\shell\TongchuanSendNAS')) {
    Remove-Item -Path $k -Recurse -ErrorAction SilentlyContinue
}
Remove-ItemProperty -Path $keys[2] -Name 'Tongchuan' -ErrorAction SilentlyContinue
# v0.20.0 起规则按程序路径建：名字固定为这两条，删除时只按名字删，不动系统里其他规则
foreach ($rn in @('通传 入站', '通传 入站（公用，限本地子网）')) {
    Get-NetFirewallRule -DisplayName $rn -ErrorAction SilentlyContinue | Remove-NetFirewallRule
}
# 清理托盘生成的 Explorer「发送到」快捷方式，只按自己的前缀匹配，不碰用户其它链接
$sendTo = [Environment]::GetFolderPath('SendTo')
if ($sendTo -and (Test-Path -LiteralPath $sendTo)) {
    Get-ChildItem -LiteralPath $sendTo -Filter '通传-发送到-*.lnk' -File -ErrorAction SilentlyContinue |
        Remove-Item -Force -ErrorAction SilentlyContinue
}
Write-Output 'OK 已清理右键菜单、SendTo 链接、登录启动与防火墙规则；用户数据未动'
