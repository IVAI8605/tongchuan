# AGENTS.md — 通传

本文件是本项目的规范与约定唯一维护位置，不引用仓库外的文件；条目随项目需要直接补充在下方。

**规范是上位约束**：本项目不得违背。确需偏离的，必须先询问用户、以用户回复为准，并登记在 `项目状态卡.md` 第 2 节；未登记的偏离算违规。

---

## 项目补充

（本项目特有的条目。只写规范没覆盖、或本项目需要具体化的内容；与规范冲突的不在此处自行豁免。）

### 版本与构建节奏

- **不按「每次改动」升版**：单端改动（例如只改 Android 界面、只改管理页文案）只在需要交付该端时构建该端，不强制重编另外两端、也不为了统一版本号而升版。
- **只有这些情况才三端同步升级**：传输协议或共用行为变化、版本门禁会拦下旧端、或用户明确要求三端一致。
- 三端各自的当前版本与协议兼容范围，维护在 `项目状态卡.md` 的「三端版本支持表」；升版时只改那一处，不再散落在各处说明里。
- 发版产物默认只留本地（项目外的本地产物归档目录），**用户点名才上传**。

### Windows 常驻启动（零可见窗口）

- 一律走 `wscript.exe //B //Nologo scripts\start-hidden.vbs`（登录 Run 键、桌面/开始菜单快捷方式、安装器装完启动都用它）。
- 不要再用 `powershell.exe -WindowStyle Hidden` 直接拉托盘：powershell 是控制台程序，会先建控制台窗口再隐藏，屏幕上仍闪一下（2026-11-15 实测，计划任务里也能看到 `DAEMON_PID ...` 闪过）。
- `start-hidden.vbs` 用 `WshShell.Run cmd, 0, False`，窗口从创建起就是隐藏的；文件以 UTF-16LE 带 BOM 保存（VBScript 按 ANSI 读会弄乱中文路径）。
- 自动化会话里必须常驻时（会话结束会回收子进程），用一次性计划任务执行上面这条 wscript 命令，跑完把任务删掉。

### 文档入口

- 当前状态与待办看 `项目状态卡.md`（本地工作状态，不入库）。
- 现行文档只看 `docs/README.md`；旧版规划、实测报告和归档已移到项目外隔离区（`cache/DSH/20261117-通传精简/旧文档/docs-old/`），需要追溯再翻。

### Linux 包构建

- 一律在 Linux 服务器本机原生编译，不做 Windows 交叉编译（本机没有 zig / musl-gcc / cargo-zigbuild，交叉编译也从未验证过）。
- 每次把当前工作区源码拷到服务器上一个干净的构建副本（例：`~/tongchuan-build-<日期>`），编译前逐文件核对哈希与仓库一致。**不要覆盖服务器上已有的旧快照/运行目录**，也不要往运行目录写构建产物。
- 命令：`CARGO_TARGET_DIR=<构建目录>/target ~/.cargo/bin/cargo build --release -p tongchuan`。非交互式 ssh 不加载登录 PATH，必须写 `~/.cargo/bin/cargo` 完整路径。
- `Cargo.toml` 的 `[profile.release] strip = true` 与 `.cargo/config.toml` 里的 `--remap-path-prefix` 必须成对保留：只 strip 不重映射，panic 里的源码绝对路径仍留在 `.rodata`。实际配置文件不入库，模板见 `.cargo/config.example.toml`。
- Windows zip / setup / APK / Linux 四个包尽量来自同一次同源构建，避免同版本出现两套不同产物。
