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
    # Windows 11 对空 SubCommands 的老式子菜单不展开；新入口改一级直达项，旧键保留但隐藏
    New-ItemProperty -LiteralPath $r -Name 'LegacyDisable' -Value '1' -PropertyType String -Force | Out-Null
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

# v0.21：Windows 11 新式右键菜单不展开旧的空 SubCommands 子菜单，增加两个一级直达项；
# 托盘启动后会按历史/发现到的设备填真实地址，缺失时显示“未发现可用目标”。
foreach ($root in @('HKCU:\Software\Classes\Directory\shell', 'HKCU:\Software\Classes\*\shell')) {
    foreach ($item in @(
        @{ Id = 'TongchuanSendPhone'; Name = '发送到手机' },
        @{ Id = 'TongchuanSendNAS'; Name = '发送到NAS' }
    )) {
        $k = Join-Path $root $item.Id
        New-Item -Path $k -Force -Value $item.Name | Out-Null
        New-ItemProperty -LiteralPath $k -Name 'MUIVerb' -Value $item.Name -PropertyType String -Force | Out-Null
        New-ItemProperty -LiteralPath $k -Name 'MultiSelectModel' -Value 'Player' -PropertyType String -Force | Out-Null
        New-Item -Path (Join-Path $k 'command') -Force -Value 'cmd /c echo 未发现可用目标 & pause' | Out-Null
    }
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

# 登录时零窗口启动托盘（托盘再拉起 daemon），不抢前台、不占用鼠标键盘。
# 必须走 wscript + start-hidden.vbs：powershell.exe 是控制台程序，即便带 -WindowStyle Hidden
# 也会先建控制台窗口再隐藏，登录/更新时会闪一下黑框（2026-11-15 实测）。
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$hidden = Join-Path $PSScriptRoot 'start-hidden.vbs'
$wscript = Join-Path $env:SystemRoot 'System32\wscript.exe'
$cmd = '"{0}" //B //Nologo "{1}"' -f $wscript, $hidden
Set-ItemProperty -Path $runKey -Name 'Tongchuan' -Value $cmd -Force
Write-Output "OK 右键子菜单 + 登录启动已写入 HKCU（隐藏窗口）"

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

# 防火墙：规则必须由提权进程创建，所以安装器已改成 RequestExecutionLevel admin。
# 为什么按程序路径而不是按端口：安装路径固定（$LOCALAPPDATA\通传\tc.exe），
# 将来加端口、改端口都不用重建规则；替换同路径的 exe 也不会再触发「是否允许访问」的询问。
# 为什么要覆盖三种配置文件：系统按「程序路径 + 网络位置」记账，漏掉一种就会在那个网络里被问一次。
$fwLog = Join-Path $InstallDir 'install-firewall.log'
function Write-FwLog([string]$line) {
    # 写文件而不是只输出：NSIS 静默调用时子进程输出只留在安装窗口，装完就没了
    $ts = Get-Date -Format 'yyyy-MM-dd HH:mm:ss'
    Add-Content -LiteralPath $fwLog -Value "$ts $line" -Encoding UTF8
}
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    # 手动以普通权限跑本脚本时走这里：不动规则，只留痕
    Write-FwLog 'SKIP 未提权，防火墙规则未改动（安装器正常安装会自动提权）'
    Write-Output 'WARN 未以管理员运行：已跳过防火墙规则'
} else {
    try {
        $ruleName = '通传 入站'
        $ruleNamePublic = '通传 入站（公用，限本地子网）'
        # 先删旧版按端口建的规则（如果历史上手动建过），避免同一程序两套口径
        foreach ($proto in @('TCP', 'UDP')) {
            & netsh advfirewall firewall delete rule name="通传 $proto 53318" *> $null
        }
        # 关键一步：系统里显式 Block 优先于 Allow。弹窗里点过「取消」会留下 Block 规则，
        # 不清掉的话新 Allow 规则形同虚设。只动与本安装路径关联、且由弹窗生成的 Block 规则。
        $prog = (Resolve-Path -LiteralPath $TcPath).Path
        $blocked = Get-NetFirewallRule | Where-Object {
            $_.Name -like '*Query User*' -and $_.Action -eq 'Block' -and
            (($_ | Get-NetFirewallApplicationFilter).Program -eq $prog)
        }
        foreach ($r in $blocked) {
            Write-FwLog "删除弹窗 Block 规则 $($r.Name)（profile=$($r.Profile)）"
            Remove-NetFirewallRule -Name $r.Name
        }
        # 幂等：先删自己建的同名规则，再按当前路径重建
        Get-NetFirewallRule -DisplayName $ruleName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
        Get-NetFirewallRule -DisplayName $ruleNamePublic -ErrorAction SilentlyContinue | Remove-NetFirewallRule
        New-NetFirewallRule -DisplayName $ruleName -Direction Inbound -Program $prog `
            -Action Allow -Protocol Any -Profile Domain,Private | Out-Null
        # 公用网络只放行本地子网：咖啡厅/热点这类公用网里不对外暴露 53318/53319
        New-NetFirewallRule -DisplayName $ruleNamePublic -Direction Inbound -Program $prog `
            -Action Allow -Protocol Any -Profile Public -RemoteAddress LocalSubnet | Out-Null
        Write-FwLog "OK 已建立入站规则：$ruleName（Domain,Private）+ $ruleNamePublic（Public, 本地子网）"
        Write-Output 'OK 防火墙入站规则已建立（专用/域全放行，公用限本地子网）'
    } catch {
        Write-FwLog "ERROR 防火墙规则未建立：$($_.Exception.Message)"
        Write-Output "WARN 防火墙规则未建立：$($_.Exception.Message)"
    }
}

# 桌面 + 开始菜单快捷方式：入口是托盘脚本（隐藏拉起 daemon），不是会挂黑窗口的 tc.exe
$shortcutScript = Join-Path $PSScriptRoot 'make-shortcut.ps1'
if (Test-Path $shortcutScript) {
    & $shortcutScript -AppDir $InstallDir -TcPath $TcPath
} else {
    Write-Output "WARN 缺少 make-shortcut.ps1，跳过快捷方式"
}
