# 通传 v0.3 Windows 安装：写 HKCU 右键子菜单、登录启动、防火墙规则。
# 只写 HKCU，不需要管理员；防火墙规则需要提权，失败只提示不中断。
# 本脚本只新增/覆盖自己创建的注册表项，不删除任何文件或注册表键。
param(
    [string]$InstallDir = '',
    [string]$TcPath = '',
    [string]$ManageUrl = 'http://127.0.0.1:53319'
)
$ErrorActionPreference = 'Continue'
if (-not $InstallDir) { $InstallDir = Split-Path -Parent (Split-Path -Parent $PSScriptRoot) }
if (-not $TcPath) { $TcPath = Join-Path $InstallDir 'tc.exe' }
if (-not (Test-Path $TcPath)) { Write-Output "ERROR tc.exe not found: $TcPath"; exit 1 }

$tray = Join-Path $PSScriptRoot 'tray.ps1'
$roots = @(
    'HKCU:\Software\Classes\Directory\shell\TongchuanSend',
    'HKCU:\Software\Classes\*\shell\TongchuanSend'
)
foreach ($r in $roots) {
    New-Item -Path $r -Force -Value '用通传发送到' | Out-Null
    # 必须 LiteralPath：路径里的 * 会被当成通配符去枚举整个 Classes 树，
    # 实测一次 New-ItemProperty 要 69 秒，安装界面看起来就是卡死（2026-11-13 实测）
    New-ItemProperty -LiteralPath $r -Name 'SubCommands' -Value '' -PropertyType String -Force | Out-Null
    $shell = Join-Path $r 'shell'
    New-Item -Path $shell -Force | Out-Null
    # 固定 5 个设备槽位，由 tray.ps1 覆写；不删旧项，避免任何破坏性操作
    for ($i = 0; $i -lt 5; $i++) {
        $k = Join-Path $shell "Device_$i"
        New-Item -Path $k -Force -Value '（未发现设备）' | Out-Null
        New-Item -Path (Join-Path $k 'command') -Force -Value 'cmd /c echo 暂无可用设备 & pause' | Out-Null
    }
    $m = Join-Path $shell 'Manage'
    New-Item -Path $m -Force -Value '打开管理页' | Out-Null
    New-Item -Path (Join-Path $m 'command') -Force -Value "powershell.exe -NoProfile -WindowStyle Hidden -Command `"Start-Process '$ManageUrl'`"" | Out-Null
}

# 「通传：再次发送上一批」：把历史里上一批路径原样再发一次（tc resend --id last --retry）。
# 带 pause 是因为 tc 是控制台程序，不暂停的话窗口一闪就关，成功失败都看不见。
# 它与「用通传发送到」并列，不放进子菜单：重发用的是历史路径，跟当前选中的文件无关。
$resendCmd = 'cmd /c ""{0}" resend --id last --retry & pause"' -f $TcPath
foreach ($r in @(
    'HKCU:\Software\Classes\Directory\shell\TongchuanResendLast',
    'HKCU:\Software\Classes\*\shell\TongchuanResendLast')) {
    New-Item -Path $r -Force -Value '通传：再次发送上一批' | Out-Null
    New-Item -Path (Join-Path $r 'command') -Force -Value $resendCmd | Out-Null
}

# 登录时隐藏启动托盘（托盘再拉起 daemon），不抢前台、不占用鼠标键盘
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$cmd = 'powershell.exe -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File "{0}" -TcPath "{1}"' -f $tray, $TcPath
Set-ItemProperty -Path $runKey -Name 'Tongchuan' -Value $cmd -Force
Write-Output "OK 右键子菜单 + 登录启动已写入 HKCU"

# 首次安装生成配置文件。安装目录里只有模板 config.example.toml，
# 缺 config.toml 时托盘与 daemon 都起不来（2026-11-13 实测：登录自启的托盘因缺配置直接退出，
# 用户看到的现象就是「装完了但没有托盘」）。已存在则一律不动，避免覆盖用户改过的设置。
$cfgPath = Join-Path $InstallDir 'config.toml'
if (-not (Test-Path $cfgPath)) {
    # 一律写绝对路径：daemon 的启动工作目录不确定，相对路径会把数据落到意想不到的位置
    $dirSlash = $InstallDir.Replace('\', '/')
    $conf = @"
alias = "$env:COMPUTERNAME"
listen = "0.0.0.0:53318"
manage = "0.0.0.0:53319"
destination = '$dirSlash/received'
allow = ['$dirSlash/received']
identity_dir = '$dirSlash/identity'
log_dir = '$dirSlash/logs'
queue_dir = '$dirSlash/queue'
log_days = 2
log_level = "info"
log_max_bytes = 4194304
auto_accept = true
trust_mode = "closed"
"@
    # 必须 UTF-8 无 BOM：TOML 解析遇到 BOM 会直接报错（模板首行也写着这条）
    [System.IO.File]::WriteAllText($cfgPath, $conf, (New-Object System.Text.UTF8Encoding $false))
    Write-Output "OK 已生成默认配置 $cfgPath"
} else {
    Write-Output "OK 配置已存在，未改动 $cfgPath"
}

# 防火墙规则需要管理员。非管理员时 netsh 每次要等十几秒才报「需要提升」，
# 四条命令累计约 78 秒，安装界面看起来像卡死（2026-11-13 实测）——所以先判权限，没权限就跳过。
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Output 'WARN 未以管理员运行：已跳过防火墙规则（需要别的设备主动连本机时，用管理员重跑 install.ps1）'
} else {
    foreach ($proto in @('TCP', 'UDP')) {
        $ruleName = "通传 $proto 53318"
        & netsh advfirewall firewall delete rule name="$ruleName" *> $null
        & netsh advfirewall firewall add rule name="$ruleName" dir=in action=allow protocol=$proto localport=53318 *> $null
        if ($LASTEXITCODE -eq 0) { Write-Output "OK 防火墙 $proto 53318" }
        else { Write-Output "WARN 防火墙 $proto 规则未添加，请用管理员运行 install.ps1" }
    }
}

# 桌面 + 开始菜单快捷方式：入口是托盘脚本（隐藏拉起 daemon），不是会挂黑窗口的 tc.exe
$shortcutScript = Join-Path $PSScriptRoot 'make-shortcut.ps1'
if (Test-Path $shortcutScript) {
    & $shortcutScript -AppDir $InstallDir -TcPath $TcPath
} else {
    Write-Output "WARN 缺少 make-shortcut.ps1，跳过快捷方式"
}
