# 右键发送菜单的集成检查：直接跑已安装桌面端的 `--refresh-menu`，再看注册表里生成的东西。
# 1.4.2 起菜单由桌面壳自己维护（不再有 scripts/windows/TcContextMenu.ps1），所以测的是真实产物。
# 运行：pwsh -NoProfile -File tests/windows/TcContextMenu.Tests.ps1
# 想测别的安装位置：先设 $env:TC_DESKTOP_EXE 指向 tongchuan-desktop.exe。
$ErrorActionPreference = 'Stop'

$exe = $env:TC_DESKTOP_EXE
if (-not $exe) {
    $exe = Join-Path $env:LOCALAPPDATA '通传\tongchuan-desktop.exe'
}
if (-not (Test-Path -LiteralPath $exe)) {
    Write-Output "SKIP 没找到桌面端 $exe（先装一次 1.4.x 安装包，或设 TC_DESKTOP_EXE）"
    exit 0
}

$refresh = Start-Process -FilePath $exe -ArgumentList '--refresh-menu' -PassThru
$refresh.WaitForExit()
Start-Sleep -Seconds 1
if ($refresh.ExitCode -ne 0) { throw "刷新菜单失败（exit=$($refresh.ExitCode)）" }

$shells = @('*', 'Directory', 'Folder')
$found = 0
foreach ($s in $shells) {
    $base = "HKCU:\Software\Classes\$s\shell"
    # `*` 在 -Path 里是通配符，必须 -LiteralPath，否则文件类型的菜单根本查不到
    if (-not (Test-Path -LiteralPath $base)) { continue }
    $items = Get-ChildItem -LiteralPath $base | Where-Object { $_.PSChildName -like 'TongchuanMenu_*' }
    foreach ($item in $items) {
        $found++
        $props = Get-ItemProperty -LiteralPath $item.PSPath
        $label = $props.MUIVerb
        if (-not $label) { $label = $props.'(default)' }
        if ($label -is [array]) { $label = $label[0] }
        if ($label -notlike '用通传发送到*') { throw "$s/$($item.PSChildName) 菜单文字不对：$label" }
        if (-not $props.Icon) { throw "$s/$($item.PSChildName) 没有设置图标" }
        if ($props.MultiSelectModel -ne 'Player') { throw "$s/$($item.PSChildName) 没开多选（MultiSelectModel=$($props.MultiSelectModel)）" }
        $cmd = (Get-ItemProperty -LiteralPath (Join-Path $item.PSPath 'command') -ErrorAction SilentlyContinue).'(default)'
        if (-not $cmd) { throw "$s/$($item.PSChildName) 缺少 command" }
        if ($cmd -notmatch '--send-to') { throw "$s/$($item.PSChildName) 命令里没有 --send-to：$cmd" }
        if ($cmd -notmatch [regex]::Escape($exe)) { throw "$s/$($item.PSChildName) 命令没指向当前桌面端：$cmd" }
        Write-Output "PASS $s/$($item.PSChildName)  $label"
    }
}
if ($found -eq 0) { throw '刷新后注册表里一个 TongchuanMenu 都没有（discover 没发现设备？先让手机/NAS 在线）' }

# 老的四条遗留键必须已被清掉，否则右键里会重复出现「（未发现设备）」
$legacy = @(
    'HKCU:\Software\Classes\*\shell\TongchuanSend',
    'HKCU:\Software\Classes\Directory\shell\TongchuanSend',
    'HKCU:\Software\Classes\Directory\shell\TongchuanSendNAS',
    'HKCU:\Software\Classes\Directory\shell\TongchuanSendPhone',
    'HKCU:\Software\Classes\Directory\shell\TongchuanResendLast'
)
foreach ($k in $legacy) {
    if (Test-Path -LiteralPath $k) { throw "遗留菜单键还在：$k" }
}
Write-Output 'PASS 遗留键已清理'
Write-Output 'ALL PASS'
