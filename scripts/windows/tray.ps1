# 通传 v0.3 Windows 托盘常驻：隐藏启动 daemon，显示托盘图标，周期刷新右键菜单设备列表。
# -Headless 时只启动 daemon，不建托盘，供离线冒烟使用。
# 全程不抢前台、不占用鼠标键盘。
param(
    [string]$TcPath = '',
    [string]$Config = '',
    [string]$ManageUrl = 'http://127.0.0.1:53319',
    [switch]$Headless,
    # v0.13.3：双击桌面图标时带上它，启动（或发现已在运行）后直接打开管理页
    [switch]$OpenUi
)
$ErrorActionPreference = 'Continue'
# 默认路径要同时认两种布局：仓库是 scripts\windows\tray.ps1，安装版是 scripts\tray.ps1。
# 只按「往上两层」找的话，安装版会指到 %LOCALAPPDATA%\ 这一级（2026-11-13 实测踩到）。
$scriptParent = Split-Path -Parent $PSScriptRoot
$appRoot = if ((Split-Path -Leaf $scriptParent) -eq 'windows') { Split-Path -Parent $scriptParent } else { $scriptParent }
if (-not $TcPath) { $TcPath = Join-Path $appRoot 'tc.exe' }
if (-not $Config) { $Config = Join-Path $appRoot 'config.toml' }
if (-not (Test-Path $TcPath)) { Write-Output "ERROR tc.exe not found: $TcPath"; exit 1 }
if (-not (Test-Path $Config)) { Write-Output "ERROR config.toml not found: $Config"; exit 1 }

# 管理页要令牌：直接打开只会得到 401 提示页。令牌文件在 identity 目录下，
# 这里读出来拼进 ?t=，点一下就进得去（令牌只出现在浏览器地址栏，不写日志）。
# 这两个函数必须定义在单实例判断之前：双击图标时托盘往往已经在跑，
# 第二个实例要先把页面打开再退出（v0.13.3）。
$appDir = Split-Path -Parent $TcPath

# 右键“发送到”入口的纯参数模块；安装版和仓库版都在同一个 scripts 目录。
$ctxModule = Join-Path $PSScriptRoot 'TcContextMenu.ps1'
if (Test-Path -LiteralPath $ctxModule) { . $ctxModule }

function Get-ManageUrl {
    $idDir = $null
    if (Test-Path $Config) {
        $m = Select-String -Path $Config -Pattern '^\s*identity_dir\s*=\s*''(.+)''' | Select-Object -First 1
        if ($m) {
            $idDir = $m.Matches[0].Groups[1].Value
            # 配置里的相对路径按配置文件所在目录展开，与 CLI 的解析口径保持一致
            if (-not [System.IO.Path]::IsPathRooted($idDir)) {
                $idDir = Join-Path (Split-Path -Parent $Config) $idDir
            }
        }
    }
    if (-not $idDir) { $idDir = Join-Path $appDir 'identity' }
    $tokenPath = Join-Path $idDir 'manage.token'
    if (Test-Path $tokenPath) {
        $tok = (Get-Content $tokenPath -Raw).Trim()
        if ($tok) { return "$ManageUrl/?t=$tok" }
    }
    return $ManageUrl
}

# 等 daemon 就绪再开页面：刚双击时进程可能还在初始化，立刻打开会看到连接被拒
function Open-ManagePage {
    for ($i = 0; $i -lt 20; $i++) {
        try {
            $null = Invoke-RestMethod -Uri "$ManageUrl/api/health" -TimeoutSec 2
            break
        } catch {
            Start-Sleep -Milliseconds 300
        }
    }
    Start-Process (Get-ManageUrl)
}

# 单实例：桌面快捷方式和登录自启动可能同时触发，只允许一个托盘 + 一个 daemon。
# 命名互斥量在进程退出时自动释放，不需要额外清理。
$isFirst = $false
$mutex = New-Object System.Threading.Mutex($true, 'Local\TongchuanTray', [ref]$isFirst)
if (-not $isFirst) {
    Write-Output 'ALREADY_RUNNING 已有托盘实例，本次不重复启动'
    # 双击桌面图标时托盘通常已经在跑：此时仍要把管理页打开，否则双击看起来没反应
    if ($OpenUi) { Open-ManagePage }
    exit 0
}

# 已经有 daemon 在跑（比如上次托盘异常退出留下的）就直接接管，不再起第二个抢同一端口。
$ownDaemon = $null
$reuse = $false
try {
    $null = Invoke-RestMethod -Uri "$ManageUrl/api/health" -TimeoutSec 2
    $reuse = $true
} catch { $reuse = $false }

if ($reuse) {
    Write-Output 'DAEMON_ALREADY_RUNNING 复用现有 daemon'
} else {
    $daemonArgs = 'daemon --config "{0}"' -f $Config
    $ownDaemon = Start-Process -FilePath $TcPath -ArgumentList $daemonArgs -PassThru -WindowStyle Hidden
    Write-Output ("DAEMON_PID " + $ownDaemon.Id)
}
$daemon = $ownDaemon
# 双击桌面图标：daemon 起完（或复用已有）就把管理页拉到浏览器
if ($OpenUi) { Open-ManagePage }

# 右键子菜单的槽位来源：优先用发送历史里的目标（带当时的指纹），
# 这样对端这次没被发现在线也照样能发；发现列表只用来给槽位补上好认的名字。
# 返回 @( name, addr, fp ) 列表，失败时返回空表，不影响其它托盘功能。
# 同时把「最近一条真实目标」的记录 id 留在 $script:LastResendId，供「再次发送上一批」用。
$script:LastResendId = ''
function Get-Targets {
    $status = $null
    try { $status = Invoke-RestMethod -Uri "$ManageUrl/api/status" -TimeoutSec 2 } catch { }
    $names = @{}
    if ($status) {
        foreach ($p in @($status.peers)) { if ($p.addr) { $names[[string]$p.addr] = [string]$p.name } }
    }
    $targets = @()
    $seen = @{}
    foreach ($line in @(& $TcPath history --limit 20 2>$null)) {
        if ($line -notmatch '^REC ') { continue }
        $to = if ($line -match '\bto=(\S+)') { $Matches[1] } else { '' }
        if (-not $to) { continue }
        # 回环目标只可能来自自动化测试：菜单里不列，也不拿它当「上一批」的候选，
        # 否则点了会对着已经关掉的测试端口重试十几分钟（2026-11-13 实测踩到）
        if ($to -match '^(127\.0\.0\.1|localhost|\[::1\]):') { continue }
        # 顺带记下最近一条真实目标的记录 id，供「再次发送上一批」使用
        if (-not $script:LastResendId -and $line -match '\bid=(\S+)') { $script:LastResendId = $Matches[1] }
        if ($seen.ContainsKey($to)) { continue }
        $fp = if ($line -match '\bpeerFp=([0-9a-fA-F]{64})') { $Matches[1] } else { '' }
        $seen[$to] = $true
        $name = if ($names.ContainsKey($to)) { $names[$to] } else { $to }
        $targets += [pscustomobject]@{ name = $name; addr = $to; fp = $fp }
    }
    # 发现到但一次都没发过的设备也留个位置，否则新设备第一回只能手动敲命令
    if ($status) {
        foreach ($p in @($status.peers)) {
            $addr = [string]$p.addr
            if (-not $addr -or $seen.ContainsKey($addr)) { continue }
            $seen[$addr] = $true
            $targets += [pscustomobject]@{ name = [string]$p.name; addr = $addr; fp = [string]$p.fingerprint }
        }
    }
    return $targets
}

# 固定 5 个槽位，只覆写不删除；设备列表来自历史 + 管理页的真实接口，不编造。
# --retry：对端锁屏、刚重启这类临时故障会自动补试，右键发完人走开也不用回来重来。
function Update-Menu {
    $targets = @(Get-Targets)
    foreach ($r in @(
        'HKCU:\Software\Classes\Directory\shell\TongchuanSend\shell',
        'HKCU:\Software\Classes\*\shell\TongchuanSend\shell')) {
        # 必须 LiteralPath：路径里的 * 会被当通配符枚举整个 Classes 树，
        # 一次 Test-Path 要几十秒，托盘每 30 秒刷一次就会一直卡在那里（2026-11-13 实测）
        if (-not (Test-Path -LiteralPath $r)) { continue }
        for ($i = 0; $i -lt 5; $i++) {
            $k = Join-Path $r "Device_$i"
            if ($i -lt $targets.Count) {
                $t = $targets[$i]
                $name = [string]$t.name
                # 有指纹就走 TLS（接收端带身份后强制 TLS）；没有指纹时交给 tc send 自己听发现报文
                if ($t.fp) {
                    $cmd = '"{0}" send --to {1} --peer-fp {2} --dir "%1" --retry' -f $TcPath, $t.addr, $t.fp
                } else {
                    $cmd = '"{0}" send --to {1} --dir "%1" --retry' -f $TcPath, $t.addr
                }
            } else {
                $name = '（未发现设备）'
                $cmd = 'cmd /c echo 暂无可用设备 & pause'
            }
            New-Item -Path $k -Force -Value $name | Out-Null
            New-Item -Path (Join-Path $k 'command') -Force -Value $cmd | Out-Null
        }
    }
    # 「再次发送上一批」的目标每次刷新时确定：优先最近一条真实目标记录。
    # 一条真实记录都没有时不要退回 last——那多半是本机测试记录，点了只会对着死端口重试。
    if ($script:LastResendId) {
        $resendCmd = 'cmd /c ""{0}" resend --id {1} --retry & pause"' -f $TcPath, $script:LastResendId
    } else {
        $resendCmd = 'cmd /c echo 发送历史里还没有成功发往真实设备的记录，先发一次再重发 & pause'
    }
    foreach ($v in @(
        'HKCU:\Software\Classes\Directory\shell\TongchuanResendLast',
        'HKCU:\Software\Classes\*\shell\TongchuanResendLast')) {
        if (-not (Test-Path -LiteralPath $v)) { continue }
        New-Item -Path (Join-Path $v 'command') -Force -Value $resendCmd | Out-Null
    }
}

# Explorer 的“发送到”菜单：SendTo 快捷方式会把所有选中项追加到 PowerShell 参数末尾，
# 由 send-from-explorer.ps1 用 ValueFromRemainingArguments 接住，因此多选文件/文件夹也不用 COM 扩展。
# 只更新链接，不删除历史链接；目标消失时旧链接可能失效，重装/清理时由 uninstall.ps1 处理。
function Update-SendToLinks {
    if (-not (Get-Command Get-TcSafeLinkName -ErrorAction SilentlyContinue)) { return }
    $targets = @(Get-Targets)
    if ($targets.Count -eq 0) { return }
    if ($targets.Count -gt 5) { $targets = $targets[0..4] }
    $sendTo = [Environment]::GetFolderPath('SendTo')
    if (-not $sendTo -or -not (Test-Path -LiteralPath $sendTo)) { return }
    $script = Join-Path $PSScriptRoot 'send-from-explorer.ps1'
    if (-not (Test-Path -LiteralPath $script)) { return }
    $powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    if (-not (Test-Path -LiteralPath $powershell)) { return }

    $shell = New-Object -ComObject WScript.Shell
    foreach ($t in $targets) {
        $name = if ($t.name) { [string]$t.name } else { [string]$t.addr }
        $safe = Get-TcSafeLinkName $name
        $link = Join-Path $sendTo ("通传-发送到-{0}.lnk" -f $safe)
        $lnk = $shell.CreateShortcut($link)
        $lnk.TargetPath = $powershell
        $sendArgs = '-NoProfile -ExecutionPolicy Bypass -File "{0}" -Target "{1}"' -f $script, $t.addr
        if ($t.fp) { $sendArgs += (' -PeerFp "{0}"' -f $t.fp) }
        $lnk.Arguments = $sendArgs
        $lnk.WorkingDirectory = $appDir
        $lnk.Description = '用通传发送到 ' + $name
        $lnk.WindowStyle = 1
        $lnk.Save()
    }
}

# Windows 11 不展开旧的空 SubCommands 子菜单，改成两个一级动词；这里按历史/发现结果填命令。
function Update-DirectMenu {
    $targets = @(Get-Targets)
    $phone = @($targets | Where-Object { [string]$_.name -match '(?i)手机|phone|vivo|android|V2551' })[0]
    $nas = @($targets | Where-Object { [string]$_.name -match '(?i)nas|fnos|飞牛|IVAI-fnOS' })[0]
    if (-not $phone -and $targets.Count -ge 1) { $phone = $targets[0] }
    if (-not $nas -and $targets.Count -ge 2) { $nas = $targets[1] }

    function SendCommandFor($t) {
        if (-not $t) { return 'cmd /c echo 未发现可用目标 & pause' }
        $cmd = '"{0}" send --to {1}' -f $TcPath, $t.addr
        if ($t.fp) { $cmd += (' --peer-fp {0}' -f $t.fp) }
        return $cmd + ' --dir "%1" --retry'
    }

    foreach ($root in @(
        'HKCU:\Software\Classes\Directory\shell',
        'HKCU:\Software\Classes\*\shell')) {
        foreach ($pair in @(
            @{ Id = 'TongchuanSendPhone'; Label = '发送到手机'; Target = $phone },
            @{ Id = 'TongchuanSendNAS'; Label = '发送到NAS'; Target = $nas }
        )) {
            $k = Join-Path $root $pair.Id
            if (-not (Test-Path -LiteralPath $k)) { continue }
            # 只改自己创建的两个一级项；不创建/删除其它 verb
            $label = if ($pair.Target) { $pair.Label } else { $pair.Label + '（未发现）' }
            New-Item -Path $k -Force -Value $label | Out-Null
            New-ItemProperty -LiteralPath $k -Name 'MUIVerb' -Value $label -PropertyType String -Force | Out-Null
            New-Item -Path (Join-Path $k 'command') -Force -Value (SendCommandFor $pair.Target) | Out-Null
        }
    }
}

if ($Headless) {
    Write-Output 'HEADLESS 只启动 daemon，等待退出'
    if ($daemon) { $daemon.WaitForExit() } else { while ($true) { Start-Sleep -Seconds 30 } }
    exit 0
}

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

# 图标查找顺序：安装目录根（安装包会把 ico 放到 tc.exe 旁边）→ 仓库 assets → 运行时现画一个。
# 之前安装版没有任何 ico，只能退回系统默认图标，看起来就像「没有图标」。
# 目录以 tc.exe 所在位置为准：安装版与仓库版都适用，不会再算错一层。
$appDir = Split-Path -Parent $TcPath
$logDir = Join-Path $appDir 'logs'
function Write-TrayLog([string]$msg) {
    try {
        New-Item -ItemType Directory -Force -Path $logDir | Out-Null
        # PS5.1 的 Add-Content 默认用 ANSI，中文会变乱码；显式 UTF-8
        Add-Content -Path (Join-Path $logDir 'tray.log') -Value ("{0}`t{1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm'), $msg) -Encoding UTF8
    } catch { }
}
$icoPath = @(
    (Join-Path $appDir 'tongchuan.ico'),
    (Join-Path $appDir 'assets\tongchuan.ico'),
    (Join-Path $PSScriptRoot '..\..\assets\tongchuan.ico')
) | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1
if (-not $icoPath) {
    # 找不到图标文件就现画：深绿圆底 + 白字「通」，保证托盘里是个能认出的图标而不是空白
    $icoPath = Join-Path $logDir 'tray.ico'
    try {
        New-Item -ItemType Directory -Force -Path $logDir | Out-Null
        $bmp = New-Object System.Drawing.Bitmap 32, 32
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $g.Clear([System.Drawing.Color]::Transparent)
        $brush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 31, 111, 92))
        $g.FillEllipse($brush, 1, 1, 30, 30)
        $font = New-Object System.Drawing.Font('Microsoft YaHei', 14, [System.Drawing.FontStyle]::Bold)
        $fmt = New-Object System.Drawing.StringFormat
        $fmt.Alignment = 'Center'
        $fmt.LineAlignment = 'Center'
        $rect = New-Object System.Drawing.RectangleF 0, 0, 32, 32
        $g.DrawString('通', $font, [System.Drawing.Brushes]::White, $rect, $fmt)
        $icon = [System.Drawing.Icon]::FromHandle($bmp.GetHicon())
        $fs = [System.IO.File]::Create($icoPath)
        $icon.Save($fs)
        $fs.Close()
        $g.Dispose()
        $bmp.Dispose()
    } catch {
        Write-TrayLog "生成图标失败：$($_.Exception.Message)"
        $icoPath = ''
    }
}
$notify = New-Object System.Windows.Forms.NotifyIcon
try {
    if ($icoPath -and (Test-Path -LiteralPath $icoPath)) {
        $notify.Icon = New-Object System.Drawing.Icon($icoPath)
    } else {
        $notify.Icon = [System.Drawing.SystemIcons]::Application
    }
} catch {
    $notify.Icon = [System.Drawing.SystemIcons]::Application
}
$notify.Text = '通传 · 后台接收中'

$ctx = New-Object System.Windows.Forms.ContextMenu
$openItem = $ctx.MenuItems.Add('打开管理页')
# 管理页里现在也能选设备、选目录、看进度（v0.10），所以这里就是「发送入口」
$openItem.add_Click({ Start-Process (Get-ManageUrl) })
# 双击图标 = 打开管理页，找不到单选项时的第二条入口
$notify.add_DoubleClick({ Start-Process (Get-ManageUrl) })
# 与右键菜单里的「再次发送上一批」同一个动作：历史里的路径原样重发，失败自动补试。
# 目标 id 与右键项共用同一个判断（跳过回环测试记录），避免两边各算一套。
function Start-ResendLast {
    if (-not $script:LastResendId) {
        [System.Windows.Forms.MessageBox]::Show('发送历史里还没有发往真实设备的记录，先正常发一次再重发。', '通传') | Out-Null
        return
    }
    Start-Process -FilePath 'cmd.exe' -ArgumentList ('/c ""{0}" resend --id {1} --retry & pause"' -f $TcPath, $script:LastResendId)
}
$resendItem = $ctx.MenuItems.Add('再次发送上一批')
$resendItem.add_Click({ Start-ResendLast })
$exitItem = $ctx.MenuItems.Add('退出')
$exitItem.add_Click({
    $notify.Visible = $false
    # 只关自己起的 daemon；复用别人起的就留着，避免把管理页一起弄掉
    if ($daemon -and -not $daemon.HasExited) { $daemon.Kill() }
    [System.Windows.Forms.Application]::Exit()
})
$notify.ContextMenu = $ctx
$notify.Visible = $true
Write-TrayLog ("启动 图标={0} 可见={1}" -f $(if ($icoPath) { $icoPath } else { '系统默认' }), $notify.Visible)
# 首次启动弹一次气泡：Windows 11 默认把新图标折叠进 ^ 里，
# 不提示的话用户会以为「托盘没有图标」。
try {
    $notify.BalloonTipTitle = '通传已在后台运行'
    $notify.BalloonTipText = '图标在右下角通知区域；看不到就点任务栏的 ^，或到「任务栏设置 → 其他系统托盘图标」打开 Windows PowerShell。'
    $notify.ShowBalloonTip(5000)
} catch { }

$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 30000
$timer.add_Tick({ Update-Menu; Update-SendToLinks; Update-DirectMenu })
Update-Menu
Update-SendToLinks
Update-DirectMenu
$timer.Start()
[System.Windows.Forms.Application]::Run()
