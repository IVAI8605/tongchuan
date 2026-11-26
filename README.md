# 通传

> 局域网内的文件夹互传工具：手机、电脑、NAS 在同一网段时直连互传整个目录，不经过任何云端或中继。

通传是一个**点对点**的文件传输工具，把整个目录（保留相对路径与空目录）直接传到另一台设备，全程走 TLS 1.3 加密。提供命令行 `tc`、Android 应用、Windows 桌面端（Tauri 2）以及面向 NAS 的飞牛 fnOS 应用包。

当前版本 **1.4.1**（桌面壳与安卓体验修复 + NAS 休眠盘发送策略；Windows / Android / fpk 三端同号重编），传输协议 **0.20**（与包版本解耦，`0.20.x` 各端互通）。

## 下载

全部产物在 [Releases](https://github.com/IVAI8605/tongchuan/releases)，同一页有 `SHA256SUMS.txt` 可核对。

| 端 | 文件 | 说明 |
| --- | --- | --- |
| Windows | `tongchuan-1.4.1-win-setup.exe` | NSIS 安装包，装完含桌面端与 `bin\tc.exe` |
| Windows（免安装） | `tongchuan-desktop-1.4.1.exe` | 单文件桌面端，需自备 `tc.exe` |
| Android | `tongchuan-1.4.1-android.apk` | arm64，正式签名，可覆盖升级 |
| 飞牛 fnOS | `tongchuan-1.4.1-linux-x64.fpk` | 在 App Center「手动安装」上传 |

## 特性

- **目录传输**：一次发送多个目录，还原相对路径与空目录。
- **加密与身份**：TLS 1.3，双方各持自签证书，靠证书指纹建立信任，不依赖 CA。
- **断点续传 + 完整性校验**：按块记录进度，中断后只补缺块；每文件 BLAKE3 校验。
- **同名处理**：内容相同则跳过（增量），不同则自动让位为 `名字 (1).扩展名`。
- **多设备并发**：可同时接收多端投递，落点冲突返回明确的 BUSY，不写坏文件。
- **自动发现**：UDP 公告，`tc discover` 列出同网段正在接收的设备及指纹。
- **历史 / 重传 / 离线队列**：`tc history`、`tc resend`，对端离线先入队再 `tc flush`。
- **内嵌管理页**：daemon 自带网页（概览、发送、记录、安全、设置），密码登录，`/api/health` 匿名。
- **小文件提速（1.3.1）**：单块小文件懒创建落盘，手机往 NAS 传大量照片更快。
- **桌面端顺手操作（1.4.1）**：收到文件右下角弹 10 秒提示、点开即落点目录；文件/文件夹拖进窗口直接发；只能开一个实例；托盘右键菜单正常。

## 支持的平台

| 平台 | 形态 |
| --- | --- |
| Windows | Tauri 2 桌面端（托盘 + 管理页窗口 + NSIS 安装包）；亦可单用命令行 `tc.exe` |
| Linux / macOS | 命令行 `tc`（本仓库主程序） |
| Android | App（常驻接收、发送、分享菜单、历史） |
| NAS（飞牛 fnOS 等） | 应用包 `.fpk`（或 `tc daemon` + systemd），浏览器管理 |

## 快速开始

```bash
# 1) 准备配置：改 destination（落点）与 allow（允许写入的目录白名单）
cp configs/config.example.toml config.toml

# 2) 起守护进程（首次启动生成设备证书与私钥）
tc daemon --config config.toml

# 3) 发现并发送
tc discover
tc send --to <IP:53318> --dir <目录> [--dir <目录> ...]
```

默认端口：**53318**（传输 TLS 1.3 + UDP 发现）、**53319**（管理页 `http://<IP>:53319/`，首次打开设置管理密码）。

> `destination` 必须在 `allow` 白名单内，否则守护进程拒绝启动——避免接收端变成任意写盘入口。

## 构建

**命令行（主程序）**

```bash
cargo build --release -p tongchuan   # 产物：tc（Windows 为 tc.exe）
```

> `target-dir` 需指向项目外的纯 ASCII 目录（中文路径会让 `ring` 链接失败）；构建细节与发布配置见 [docs/README.md](./docs/README.md)。

**Android**：先在 Linux 交叉编译 `libtc_jni.so` 放入 `android/app/src/main/jniLibs/arm64-v8a/`，再用 Gradle 出包；正式签名通过环境变量 `TC_KEYSTORE_PROPS` 指向 `keystore.properties` 启用（未设置则退回 debug）。

**Windows 桌面端**：在 Windows 构建机上执行 `pwsh scripts\build-desktop.ps1`（会自动编译 `tc.exe`、校验版本、再出 NSIS 安装包）；只想手动出壳则 `cd windows-gui\src-tauri && npx @tauri-apps/cli@2 build`。

**NAS 飞牛 fpk**：`python3 fpk/build-fpk.py`。各端完整命令见 [docs/README.md](./docs/README.md)。

## 安全模型

- 自签证书 + 指纹固定，不校验证书链（局域网 IP 会变），首次连接需人工确认指纹。
- 信任默认 **`closed`**（1.3.3 起）：陌生设备首次发送要在管理页扫码配对或手动批准；需要「同网段免配对」时可在管理页或安装向导切到宽松模式 `open`（仅建议自用可信网络）。安卓侧载版为不打断日常使用仍默认 `open`，Windows 桌面端与 fpk 默认 `closed`。
- 落点受 `allow` 白名单限制，路径逐段清洗，拒绝 `..` 与越界写入。
- 管理页用密码登录，不区分用户；不要直接暴露到公网。

## 常见问题

- **管理页要密码，忘了怎么办**：删掉 `identity/manage.pwd` 再打开页面即可重设（Windows：`%APPDATA%\com.tongchuan.desktop\identity\manage.pwd`；飞牛：`/vol1/@appdata/tongchuan/identity/manage.pwd`）。1.3.x 及更早用「令牌链接」登录，升级到 1.4 后第一次会要求设密码，属正常。
- **右下角提示看不到 / 位置奇怪**：1.4.1 起提示跟随鼠标所在的那块屏；装了串流、投屏类虚拟显示器（Windows 把它们设成「主屏」）时不会再弹到屏幕外。
- **NAS 盘休眠，头一个文件卡很久**：1.4.1 起发送前会并行预热源盘，并对读块做退避重试，只是盘起转需要几秒，不会把整场发送打断。
- **端口被占用 / 开了两个窗口**：1.4.1 起桌面端只允许一个实例；若接收起不来，先查 53318/53319 是否被别的东西占了。
- **手机点「最近接收」没反应 / 列表不刷新**：1.4.1 起收完文件列表当场刷新（按接收记录变化判断，几百毫秒的小传输也不会漏），点条目直接交给默认应用打开。原先有两处毛病一并修了：URI 二次转义导致 `openFile` 找不到文件；Provider 不返回文件名/大小，阅读器只能显示「file」还要走「导入并存储」。仍打不开说明系统没有能打开该类型的应用。

## 版本与兼容

- 版本号唯一来源：[Cargo.toml](./Cargo.toml) 的 `workspace.package.version`，CLI、Android、Windows、fpk 都从它读，不手写第二份；三端同号。
- 传输协议固定在 `crates/tc-core/src/session.rs` 的 `0.20`，与包版本解耦；版本门禁只比较主次版本，`0.20.x` 各端互通。

## 文档

- [CHANGELOG.md](./CHANGELOG.md)：逐版本改动与实测记录
- [docs/README.md](./docs/README.md)：开发与构建速查
- 内部部署与运维文档不随仓库分发

## 许可

[MIT](./LICENSE) © IVAI8605。可自由使用、修改与再分发，保留版权与许可声明即可；软件按「原样」提供，不附带担保。
