# 通传

局域网内的文件夹互传工具。手机、电脑、NAS 在同一网段时，直接用一条命令或点几下屏幕，把整个目录（保留相对路径与空目录）传到另一端；不经过任何云端或中继。

当前版本：`1.3.1`（版本号唯一来源是根目录 [Cargo.toml](./Cargo.toml) 的 `workspace.package.version`）。协议版本与包版本解耦，当前协议 `0.20`，与 0.20.x 各端互通。文档索引见 [docs/README.md](./docs/README.md)。

## 能做什么

- **目录传输**：一次发送多个目录，接收端还原相对路径结构，空目录也会还原。
- **加密与身份**：传输走 TLS 1.3，双方各持自签证书；首次连接靠证书指纹建立信任，之后按指纹放行，不依赖 CA。信任表是追加式事件文件，批准与撤销即时生效。
- **断点续传**：接收端按落点路径记录已完成的块（同目录下的 `.tcpart.map`），中断后重发只补缺块。
- **完整性校验**：每个文件传完做 BLAKE3 校验，结果写入日志与历史。
- **同名处理**：同一落点已有同名文件时按内容判断——内容相同则跳过（增量），不同则自动让位为 `名字 (1).扩展名`。
- **并发会话**：多台设备可同时投递；同一落点被占用时后来者会收到明确的 BUSY，而不是写坏文件。
- **发现**：UDP 组播/广播/回环公告，`tc discover` 可列出局域网内正在接收的设备及其指纹。
- **发送历史与重传**：`tc history` / `tc resend`，历史只追加不删行。
- **离线队列**：对端暂时不可达时先入队，`tc flush` 再补发。
- **管理页**：守护进程自带内嵌网页，看概览/发送/历史与日志/安全（信任·配对）/设置。首次打开设置管理密码，之后密码登录；`/api/health` 匿名。
- **Windows 桌面端**：Tauri 2 原生桌面应用——托盘常驻 + 管理页窗口，托盘菜单可「打开管理页 / 重启接收 / 退出」；关窗缩托盘继续接收。
- **剪贴板中继（可选，默认关闭）**：核心 CLI 保留该能力；安卓端与 Windows 桥接脚本已移除。
- **诊断**：`tc diag` 一键导出配置、日志与历史，便于排障。

三端形态：

| 端 | 形态 |
|---|---|
| Windows | Tauri 2 桌面应用（`windows-gui/`：原生托盘 + 管理页窗口 + NSIS 安装包）；命令行 `tc` 亦可单独用 |
| Linux / macOS | 命令行 `tc`（本仓库主程序） |
| Android | `android/` 下的 App：常驻接收（前台服务 + 无障碍兜底）、发送、分享菜单、历史 |
| 无头服务器（NAS 等） | 飞牛 fnOS 应用包 `src/fpk/`（或 `tc daemon` + systemd）；管理页浏览操作 |

## 目录结构

```
src/                 CLI 入口（tc 的可执行目标）
crates/tc-core/      协议、TLS、传输、续传、信任库、历史、剪贴板、管理页
crates/tc-jni/       Android 侧 JNI 绑定
android/             Android 应用（Kotlin + Compose + Gradle）
configs/             配置模板与 systemd unit 模板
scripts/             构建与验证脚本（verify.ps1、android/）
windows-gui/         Windows 桌面端（Tauri 2：原生托盘 + 管理页窗口）
tests/               集成测试（端到端、并发、信任门禁）
docs/                开发速查（旧规划/报告已移入隔离区）
assets/              图标等静态资源
```

## 构建

前置：Rust stable（`cargo` 在 PATH 中）。Windows 上构建还需要 MSVC 或 GNU 工具链之一。

```bash
cargo build --release --workspace
```

产物是 `tc`（Windows 为 `tc.exe`）。

注意两点：

- 构建目录走 `.cargo/config.toml`。仓库只带模板 [.cargo/config.example.toml](./.cargo/config.example.toml)：先复制成 `.cargo/config.toml`（该文件不入库），再把 `target-dir` 指到项目之外的纯 ASCII 目录——仓库目录名是中文时，`ring` 的 `ar`/链接器打不开中文路径下的产物。报 `cannot open output file ... No such file or directory` 时，直接用环境变量覆盖：`export CARGO_TARGET_DIR=/vol1/1000/SSD/DSH/AI/_tool/build/tongchuan-target`（NAS 上统一用这个；**不要用系统盘 `/tmp`**，32G 盘会被构建写爆）。模板里还说明了发布构建为什么要同时开 `strip` 与路径重映射。
- 版本号只在根 `Cargo.toml` 写一次，CLI、Android 应用、安装包都从那里读，不要手写第二份。

### Android

前置：JDK 17、Android SDK（`compileSdk 35` / build-tools 36.0.0）、NDK 27.3、Gradle 8.11.1。

**NAS / Linux（当前实际构建方式，2026-11-19 起）**：见 [docs/README.md](./docs/README.md) 的「常用命令」——
先交叉编译出 `libtc_jni.so` 拷进 `android/app/src/main/jniLibs/arm64-v8a/`，再用项目内可写缓存跑 Gradle。
`scripts/android/build-apk.ps1` 是 Windows 历史脚本（找 `~/.gradle/wrapper` 下的 gradle.bat、默认 `--offline`、NDK 路径写死 windows），**在 NAS 上跑不通**，别用它。

只用 `arm64-v8a`。Release 包复用调试签名，属于自用侧载定位；签名 key 的恢复流程见 [docs/README.md](./docs/README.md)。

### Windows 桌面端（Tauri 2）

在 Windows 构建机（需 Rust + Node + WebView2）：

```powershell
cd windows-gui\src-tauri
npx @tauri-apps/cli@2 build   # 产物：target\release\bundle\nsis\通传_<版本>_x64-setup.exe
```

### 跑一遍门禁

`scripts/verify.ps1` 会依次做：构建 → `cargo test` → 命令行 TLS 回环 → 管理页接口冒烟 → 脚本产物检查 → 版本一致性 → 历史与重传 → 信任库。它只回 PASS/FAIL 摘要，完整日志留在 `cache/` 下。

```powershell
& pwsh -NoProfile -File scripts/verify.ps1
```

## 运行

### 1. 准备配置

```bash
cp configs/config.example.toml config.toml
```

改三件事就够：`destination`（落点）与 `allow`（允许写入的目录白名单，落点必须在其中）、各类数据目录、`manage`（管理页监听地址）。`destination` 不在 `allow` 内时守护进程会拒绝启动——这是防止接收端变成任意写盘入口的硬约束。

### 2. 起守护进程

```bash
tc daemon --config config.toml
```

首次启动会在 `identity_dir` 下生成设备证书与私钥；管理密码哈希存 `identity/manage.pwd`（首次打开网页时设置）。默认端口：

| 端口 | 协议 | 用途 |
|---|---|---|
| 53318 | TCP + UDP | 传输（TLS 1.3）与发现公告 |
| 53319 | HTTP | 管理页 |
| 53320 | TCP | 剪贴板中继（仅当配置里开启 `clip_listen`） |

管理页地址 `http://<本机地址>:53319/`：首次打开要求设置管理密码，之后密码登录（HttpOnly 会话 Cookie）。密码只存加盐哈希 `identity/manage.pwd`（0600）；忘了可在数据目录删除 `manage.pwd` 重设。

长期运行时用 `configs/tongchuan.service` 作模板装成 systemd 服务，按实际用户与卷路径改 `User`、`WorkingDirectory`、`ReadWritePaths`。

### 3. 配对与发送

```bash
# 列出正在接收的设备和它们的指纹
tc discover

# 第一次发送：对端会把你记为「待批准」并拒绝，接收端执行
tc trust pending
tc trust approve <64位指纹> --alias <设备别名>

# 再发
tc send --to <IP:端口> --dir <目录> [--dir <目录>...]

# 也可以手动起一个一次性接收端
tc recv --listen 0.0.0.0:53318 --dest <落点> --pair-code 123456
```

其余子命令见 `tc`（不带参数）打印的用法，或：

```bash
tc clip serve|push|status|watch|pause|resume|history   # 剪贴板
tc diag                                                 # 导出诊断包
```

## 安全模型要点

- 自签证书 + 证书指纹固定，不做域名校验（局域网 IP 会变）。首次连接必须人工确认指纹。
- 信任模式默认 `open`（安全功能默认关闭 = 同网段发现即可传）；开启「严格配对」才切 `closed` 按信任表放行；open 模式只放行、不写信任表。
- 落点被 `allow` 白名单限制，路径逐段清洗，拒绝 `..` 与越界写入。
- 剪贴板中继默认关闭——剪贴板里可能有密码，开启前请自行评估。
- 管理页用密码登录、不区分用户；不要直接暴露到公网。

## 文档

- [CHANGELOG.md](./CHANGELOG.md)：逐次改动与实测记录
- `docs/`：当前开发速查；旧规划、报告和归档已移到 `cache/DSH/20261117-通传精简/旧文档/docs-old/`
- 内部部署与运维文档不随仓库分发

## 许可

本项目使用 MIT 许可证，全文见 [LICENSE](./LICENSE)。可以自由使用、修改与再分发，保留版权声明与许可声明即可。软件按「原样」提供，不附带任何担保。
