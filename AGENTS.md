# AGENTS.md — 通传

本文件是本项目的规范与约定唯一维护位置，不引用仓库外的文件；条目随项目需要直接补充在下方。

**规范是上位约束**：本项目不得违背。确需偏离的，必须先询问用户、以用户回复为准，并登记在 `项目状态卡.md` 第 2 节；未登记的偏离算违规。

---

## 项目补充

（本项目特有的条目。只写规范没覆盖、或本项目需要具体化的内容；与规范冲突的不在此处自行豁免。）

### Linux 包构建

- 一律在 Linux 服务器本机原生编译，不做 Windows 交叉编译（本机没有 zig / musl-gcc / cargo-zigbuild，交叉编译也从未验证过）。
- 每次把当前工作区源码拷到服务器上一个干净的构建副本（例：`~/tongchuan-build-<日期>`），编译前逐文件核对哈希与仓库一致。**不要覆盖服务器上已有的旧快照/运行目录**，也不要往运行目录写构建产物。
- 命令：`CARGO_TARGET_DIR=<构建目录>/target ~/.cargo/bin/cargo build --release -p tongchuan`。非交互式 ssh 不加载登录 PATH，必须写 `~/.cargo/bin/cargo` 完整路径。
- `Cargo.toml` 的 `[profile.release] strip = true` 与 `.cargo/config.toml` 里的 `--remap-path-prefix` 必须成对保留：只 strip 不重映射，panic 里的源码绝对路径仍留在 `.rodata`。实际配置文件不入库，模板见 `.cargo/config.example.toml`。
- Windows zip / setup / APK / Linux 四个包尽量来自同一次同源构建，避免同版本出现两套不同产物。
