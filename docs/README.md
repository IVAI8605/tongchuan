# 通传 · 开发速查

> 日常开发与构建看这份；产品介绍见根目录 [README](../README.md)，逐版本改动见 [CHANGELOG](../CHANGELOG.md)。
> 旧的规划、实测报告与历史说明已移到项目外隔离区（`cache/DSH/20261117-通传精简/旧文档/docs-old/`），需要追溯再翻。

## 版本与协议

- 版本号唯一来源：根 `Cargo.toml` 的 `workspace.package.version`（当前 `1.4.0`；本次只交付 CLI，Windows / Android / fpk 仍 1.3.1、未重编；协议仍 `0.20`）。
- 传输协议固定在 `crates/tc-core/src/session.rs` 的 `0.20`，与包版本解耦；门禁只比主次版本，`0.20.x` 各端互通。

## 目录

| 路径 | 用途 |
|---|---|
| `crates/tc-core` | 传输协议、会话、TLS、续传、信任库、历史、管理页 |
| `crates/tc-jni` | Android JNI 门面 |
| `android/` | Android 客户端（Kotlin/Java + Compose，包名 `com.tongchuan`） |
| `windows-gui/` | Windows 桌面端（Tauri 2：原生托盘 + 管理页窗口 + NSIS 安装包） |
| `fpk/` | 飞牛 fnOS 应用包打包脚本 |
| `configs/` | 配置模板与 systemd unit 模板 |
| `tests/` | 集成测试（端到端、并发、信任门禁） |

运行数据在项目根的 `identity/`、`logs/`、`queue/`、`received/`（含设备私钥），不入库。

## 常用命令

```bash
# NAS 上先加载工具 PATH（source 后 $AI_TOOL / adb / gradle / NDK 可用）
source "$AI_TOOL/env.sh"

cargo test --workspace                 # 全部测试
cargo build --release -p tongchuan     # 出 tc（Windows 为 tc.exe）
pwsh -NoProfile -File scripts/verify.ps1   # 门禁：构建→测试→回环→管理页冒烟→产物/版本校验
```

> `target-dir` 要指向项目外的纯 ASCII 目录（中文路径会让 `ring` 链接失败）；别用系统盘 `/tmp`。

## Android 构建

1. 交叉编译 Rust 内核为 `libtc_jni.so`，拷进 `android/app/src/main/jniLibs/arm64-v8a/`（`.so` 不入库，干净 clone 后必做）：

```bash
NDK_BIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
CC_aarch64_linux_android="$NDK_BIN/aarch64-linux-android24-clang" \
CXX_aarch64_linux_android="$NDK_BIN/aarch64-linux-android24-clang++" \
AR_aarch64_linux_android="$NDK_BIN/llvm-ar" \
CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$NDK_BIN/aarch64-linux-android24-clang" \
CARGO_TARGET_DIR=<项目外纯ASCII目录>/tongchuan-android-target \
cargo build --release -p tc-jni --target aarch64-linux-android
cp <...>/aarch64-linux-android/release/libtc_jni.so android/app/src/main/jniLibs/arm64-v8a/
```

2. Gradle 打包（用项目内可写缓存；只用 `arm64-v8a`）：

```bash
cd android
GRADLE_USER_HOME=$PWD/cache/gradle-home gradle --no-daemon :app:assembleRelease :app:testDebugUnitTest
```

**签名**：release 默认走正式密钥——构建时用环境变量 `TC_KEYSTORE_PROPS` 指向一份 `keystore.properties`（含 `storeFile/storePassword/keyAlias/keyPassword`）即启用；未设置则退回 debug（开发包）。密钥与密码放在仓库外受保护目录（如项目根 `_keys/android/`），不入库。

## Windows 构建

在 Windows 构建机（需 Rust + Node + WebView2）：

```powershell
# 先出 tc.exe 放进 windows-gui\src-tauri\bin\ ：
cargo build --release -p tongchuan
copy target\release\tc.exe windows-gui\src-tauri\bin\tc.exe
# 再出 NSIS 安装包：
cd windows-gui\src-tauri && npx @tauri-apps/cli@2 build
```

## NAS 飞牛 fpk

```bash
python3 fpk/build-fpk.py      # 产物：dist/tongchuan-<版本>-linux-x64.fpk（版本读 Cargo）
```

## 约定

- push / GitHub Release 需用户明确授权。
- 清理只移不删，移动前留清单；不写 `identity/identity.key`、`config.toml`、`.cargo/config.toml` 与运行时数据。
- 构建产物放项目外；仓库只保留源码、脚本、模板与文档。
