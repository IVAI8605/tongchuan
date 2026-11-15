; 通传 NSIS 安装脚本。版本号由 -DVERSION 传入，不再写死在这里。
Unicode true
!define APPNAME "通传"
; 版本号与 tc.exe 路径不再手写：由 scripts/windows/make-installer.ps1 从根 Cargo.toml
; 与构建配置派生后以 -D 传入。缺参直接编译失败，避免打出错版本或空包的安装器。
!ifndef VERSION
  !error "缺少 -DVERSION：请用 scripts/windows/make-installer.ps1 编译"
!endif
!ifndef TC_BUILD
  !error "缺少 -DTC_BUILD：请指向实际构建出的 tc.exe（make-installer.ps1 会自己算）"
!endif
; 输出目录同样由脚本传入，避免安装包落回仓库目录变成未跟踪文件
!ifndef OUT_DIR
  !error "缺少 -DOUT_DIR：请指向安装包输出目录（make-installer.ps1 会自己算）"
!endif

Name "${APPNAME} ${VERSION}"
OutFile "${OUT_DIR}\通传-${VERSION}-setup.exe"
InstallDir "$LOCALAPPDATA\通传"
; v0.20.0：改为 admin。防火墙规则必须由提权进程创建，
; 之前用 user 时 install.ps1 里的管理员判断恒为假，规则从未建立（UAC「从不通知」也一样，
; 令牌仍是筛选后的标准用户）。同一账号提权后 $LOCALAPPDATA 与 HKCU 仍指向本用户。
RequestExecutionLevel admin
SetCompressor /SOLID lzma

Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

Section "主程序"
  SetOutPath "$INSTDIR"
  File "${TC_BUILD}"
  SetOutPath "$INSTDIR\scripts"
  File /r "..\windows\*.ps1"
  SetOutPath "$INSTDIR"
  ; 配置模板 2026-11-13 起在项目根 configs\ 下（规范 S8），安装副本仍落在 $INSTDIR 根
  File "..\..\configs\config.example.toml"
  ; 托盘图标必须随包安装：安装版没有 assets 目录，缺 ico 时托盘只能退回系统默认图标
  File "..\..\assets\tongchuan.ico"

  ; 只写 HKCU 的右键菜单与登录启动，不需要管理员。
  ; 这一步是静默执行的（没有窗口输出），必须先写一行进度，否则界面停在最后一条 Extract 上像卡死。
  DetailPrint "正在写入右键菜单、登录启动与默认配置…"
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\scripts\install.ps1" -InstallDir "$INSTDIR" -TcPath "$INSTDIR\tc.exe"'
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Tongchuan" "DisplayName" "${APPNAME} ${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Tongchuan" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Tongchuan" "DisplayVersion" "${VERSION}"
  ; 装完直接把托盘拉起来，用户马上能看到图标（用 Exec 不等待，否则安装器会一直等托盘退出）
  DetailPrint "正在启动托盘…"
  Exec 'powershell.exe -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File "$INSTDIR\scripts\tray.ps1" -TcPath "$INSTDIR\tc.exe"'
SectionEnd

Section "Uninstall"
  ; 卸载脚本默认只列清单，需 -Confirm 才真正清理（遵守「不自行删除」）
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\scripts\uninstall.ps1" -Confirm'
  Delete "$INSTDIR\uninstall.exe"
  Delete "$INSTDIR\tc.exe"
  Delete "$INSTDIR\config.example.toml"
  Delete "$INSTDIR\tongchuan.ico"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Tongchuan"
SectionEnd
