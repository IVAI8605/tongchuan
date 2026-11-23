# 通传 · 开发速查

> 这份就够日常看。旧的规划、实测报告和历史版本说明已移到项目外的隔离区
> （`cache/DSH/20261117-通传精简/旧文档/docs-old/`），需要追溯再去翻。

## 项目

局域网文件夹互传工具：电脑、手机、NAS 之间走文件通道传目录，不走云端。
源码在本仓库根目录，Linux 运行二进制在项目上一级（NAS 部署位）。

当前版本：`1.3.0`（**三端统一**，唯一来源 `Cargo.toml` 的 `workspace.package.version`）。
协议：`0.20`，已与包版本解耦（`tc-core` 里固定为 `0.20`）；三端 1.3.0（安卓 / Windows / 飞牛 fpk）与 0.20.x 各端互通。

## 目录

| 路径 | 用途 |
|---|---|
| `crates/tc-core` | 传输协议、会话、信任、剪贴板内核 |
| `crates/tc-jni` | Android JNI 门面 |
| `android/` | Android 客户端（包名 `com.tongchuan`） |
| `src/windows-gui/` | Windows 桌面端（Tauri 2：原生托盘 + 管理页窗口 + NSIS 安装包） |
| `configs/` | 配置模板；本机 `config.toml` 不入库 |
| `tests/windows/` | Windows 回归测试（旧 PowerShell 版，已随旧版搁置） |
| `../dist/通传-1.3.0.apk` | 最近的 Android 交付包（本地产物，不提交） |

运行数据在项目根的 `identity/`、`logs/`、`queue/`、`received/`，含设备私钥，不入库、不重构。

## 常用命令

```bash
# 每次先加载 NAS 工具 PATH（source 后 $AI_TOOL 可用）
source "$AI_TOOL/env.sh"
cd "$(dirname "$0")/.."   # 或直接 cd 到仓库根 src/

cargo test --workspace
pwsh -NoProfile -File scripts/verify.ps1

# ---- Android（NAS 侧完整流程，2026-11-19 实测）----
# 1) 交叉编译 Rust 内核（协议常量改动后必须重编，否则 .so 与 Java 侧不匹配）。
#    .so 被 .gitignore 忽略，干净 clone 后这一步不可省，否则 APK 缺 native 库、一启动就崩。
NDK_BIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
CC_aarch64_linux_android="$NDK_BIN/aarch64-linux-android24-clang" \
CXX_aarch64_linux_android="$NDK_BIN/aarch64-linux-android24-clang++" \
AR_aarch64_linux_android="$NDK_BIN/llvm-ar" \
CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$NDK_BIN/aarch64-linux-android24-clang" \
CARGO_TARGET_DIR=/vol1/1000/SSD/DSH/AI/_tool/build/tongchuan-android-target \
cargo build --release -p tc-jni --target aarch64-linux-android
cp /vol1/1000/SSD/DSH/AI/_tool/build/tongchuan-android-target/aarch64-linux-android/release/libtc_jni.so \
   android/app/src/main/jniLibs/arm64-v8a/

# 2) Gradle 打包：共享 gradle 缓存本机无法初始化 native services，
#    用项目内可写缓存 + PartZ 的只读依赖缓存（Compose/AGP/Kotlin 都在里面）；
#    zxing core 3.5.3 只在本项目缓存里，首次构建需联网从 aliyun 补。
cd android
JAVA_TOOL_OPTIONS="-Duser.home=$PWD/cache/home" \
GRADLE_USER_HOME=$PWD/cache/gradle-home \
GRADLE_RO_DEP_CACHE="$(dirname "$PWD")/../PartZ/cache/gradle-home/caches" \
gradle --no-daemon :app:assembleRelease :app:testDebugUnitTest

# 3) 签名 key 恢复（换机或清了 android/cache/ 后必做）：
#    release 复用 debug 签名，key 原件在 ~/.android/debug.keystore（不入库）。
#    缺失时 Gradle 会静默生成新 key → 新包装不上已装机，只能卸载重装 → 用户数据丢失。
mkdir -p cache/home/.android
cp ~/.android/debug.keystore cache/home/.android/
test -s cache/home/.android/debug.keystore || { echo "签名 key 缺失，停止构建"; exit 1; }

# Windows 打包（在 Windows 侧跑）
cd src/windows-gui/src-tauri && npx @tauri-apps/cli@2 build
```

## 当前功能

- 文件/文件夹发送：`tc send --to <addr> --peer-fp <fp> --dir <path>`；Android/Windows 客户端封装。
- Windows 桌面端（Tauri 2）：托盘常驻 + 管理页窗口；在管理页里发送、看历史、管理安全。旧版右键发送（VBS）已随旧版移除。
- 剪贴板：核心 CLI `tc clip` 保留；安卓端剪贴板同步已移除。
- 截屏同步：Android `ScreenshotWatcher` 监听 `Pictures/Screenshots`，新截图走文件通道发出；Windows 侧写剪贴板脚本已随旧版移除。

## 约定

- 发版 push / GitHub Release 需明确授权（v1.3.0 已授权发布）。
- 清理只移不删，移动前留清单；不写 `identity/identity.key`、`config.toml`、`.cargo/config.toml`、运行时数据。
- 构建产物放项目外；仓库只保留源码、脚本、模板和这份速查。
