# 通传 · 开发速查

> 这份就够日常看。旧的规划、实测报告和历史版本说明已移到项目外的隔离区
> （`cache/DSH/20261117-通传精简/旧文档/docs-old/`），需要追溯再去翻。

## 项目

局域网文件夹互传工具：电脑、手机、NAS 之间走文件通道传目录，不走云端。
源码在本仓库根目录，Linux 运行二进制在项目上一级（NAS 部署位）。

当前版本：`0.20.1`（唯一来源 `Cargo.toml` 的 `workspace.package.version`）。
协议：`0.20`；主次版本相同的 0.20.x 可互通。Android 当前 `0.20.1`，Windows `tc.exe` 仍为 `0.20.0`，可接。

## 目录

| 路径 | 用途 |
|---|---|
| `crates/tc-core` | 传输协议、会话、信任、剪贴板内核 |
| `crates/tc-jni` | Android JNI 门面 |
| `android/` | Android 客户端（包名 `com.tongchuan`） |
| `scripts/windows/` | Windows 托盘、右键菜单、隐藏发送、截屏剪贴板桥 |
| `configs/` | 配置模板；本机 `config.toml` 不入库 |
| `tests/windows/` | PowerShell 回归测试 |
| `../dist/通传-0.20.1.apk` | 最近的 Android 交付包（本地产物，不提交） |

运行数据在项目根的 `identity/`、`logs/`、`queue/`、`received/`，含设备私钥，不入库、不重构。

## 常用命令

```bash
# 先加载 NAS 工具 PATH
source <工具目录>/env.sh
cd <仓库根目录>

cargo test --workspace
pwsh -NoProfile -File scripts/verify.ps1

# Android
gradle --offline --no-daemon -p android assembleRelease

# Windows 打包（在 Windows 侧跑）
pwsh -NoProfile -File scripts/windows/make-installer.ps1
```

## 当前功能

- 文件/文件夹发送：`tc send --to <addr> --peer-fp <fp> --dir <path>`；Android/Windows 客户端封装。
- Windows 右键：`通传发送到手机` / `通传发送到NAS`，走 `wscript + send-hidden.vbs`，无命令窗。
- 剪贴板：文本同步走 `tc clip`；手机截屏自动发送见下。
- 截屏同步（0.20.1）：Android `ScreenshotWatcher` 监听 `Pictures/Screenshots`，新截图走文件通道到电脑；Windows `screenshot-clipboard.ps1` 把收到的 `Screenshot*` 图片写入剪贴板。

## 约定

- 只本地提交，未经明确允许不 push、不建 Release。
- 清理只移不删，移动前留清单；不写 `identity/identity.key`、`config.toml`、`.cargo/config.toml`、运行时数据。
- 构建产物放项目外；仓库只保留源码、脚本、模板和这份速查。
