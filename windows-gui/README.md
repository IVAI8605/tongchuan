# 通传 Windows 桌面端（Tauri 2）

正规 Windows 桌面形态：原生托盘 + 管理页窗口，后台拉起 `tc.exe daemon`。
取代原来的 PowerShell 托盘 / VBS 启动 / 注册表右键拼装。

## 目录

```
windows-gui/
├── src-tauri/          Tauri 应用（Rust）
│   ├── Cargo.toml
│   ├── tauri.conf.json  productName=通传 version=0.21.0 bundle=nsis
│   ├── capabilities/    窗口/托盘权限
│   ├── icons/icon.ico
│   ├── bin/tc.exe       构建前放入（来自 cargo build -p tongchuan）
│   └── src/main.rs      托盘 + 窗口 + 启动/停止 daemon
└── web/index.html       占位前端（窗口实际加载本机 http://127.0.0.1:53319）
```

## 构建（在 Windows 构建机）

```powershell
# 1) 出 Windows 版 tc.exe
cargo build --release -p tongchuan
copy target\release\tc.exe windows-gui\src-tauri\bin\tc.exe

# 2) 出安装包（首次会下载 tauri-cli 与依赖）
cd windows-gui\src-tauri
npx @tauri-apps\cli@2 build
# 产物：target\release\bundle\nsis\通传_0.21.0_x64-setup.exe
```

## 形态

- 启动即拉起 `tc.exe daemon`（config.toml 在应用数据目录首次生成，落点 `received\`、信任默认 open）
- 托盘菜单：打开管理页 / 重启接收 / 退出
- 关窗口只是隐藏到托盘，不影响后台接收
- 管理页复用网页版（密码登录），不再另写一套 UI
