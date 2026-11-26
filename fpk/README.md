# 通传 · NAS fpk 包（飞牛 fnOS 应用）

把通传打包成飞牛 OS（fnOS）可安装的 `.fpk`，由应用中心管理（安装 / 启停 / 升级 / 卸载），取代「手工 systemd + 裸二进制」的部署方式。

## 版本

- fpk 版本取自根 `Cargo.toml` 的 `workspace.package.version`（与三端统一；当前 `1.3.x`），可用环境变量 `TC_FPK_VERSION` 覆盖。
- 内含的 `tc` 二进制取自当前源码的 release 构建。

## 构建

```bash
# 1) 先出 tc 二进制（release）
CARGO_TARGET_DIR=<项目外纯ASCII目录>/tongchuan-target cargo build --release -p tongchuan
# 2) 再打 fpk（纯 Python 标准库，无外部依赖）
python3 fpk/build-fpk.py
```

产物：`dist/tongchuan-<版本>-linux-x64.fpk` 与同名 `.sha256`。

## 安装 / 上架

- **手动安装**：飞牛「应用中心 → 手动安装」上传 fpk；无签名的第三方 fpk 需在网页版 App Center 勾选「签名风险」确认后才能装。
- **上架应用中心**：提交同一份 fpk；包内不写死任何私有卷路径，默认接收落点放在应用自己的数据目录，符合通用分发。

## 默认行为

- 首次 `start` 在应用数据目录生成可编辑的 `config.toml`（端口、接收落点都在里面）。
- **接收落点**：落在应用的**共享文件夹「tongchuan」**下（系统创建、文件管理器可见、并 ACL 授权给应用用户）。**首次安装可命名子目录**（默认「通传接收」）。之后也能改 `config.toml` 的 `destination` 并同步 `allow`，再 `cmd/main restart`。
- **运行身份**：以专用包用户运行（`run-as: package`），不申请 root。
- **管理页**：`http://<NAS>:53319/`，**首次打开设置管理密码，之后密码登录**（`/api/health` 匿名）。
- **安全默认开启**（`trust_mode=closed`）：陌生设备首次发送需在管理页生成二维码配对或手动批准，**不免鉴权**（上架要求）。安装向导可选「宽松模式」（`open`，同网段发现即传，仅自用）。
- **卸载不删数据**：`uninstall_callback` 只提示数据位置，身份 / 信任 / 已收文件都保留，重装可继续用。

## 包结构（照 fnOS fpk 规范）

```
fpk = gzip(tar)
├── manifest            appname/version/display_name/desc/service_port/checksum(=app.tgz 的 md5)
├── app.tgz             bin/tc + ui/config + ui/images/icon_*.png
├── cmd/*               install_init / install_callback / main / uninstall_* / upgrade_*（带可执行位）
├── config/privilege    run-as
├── config/resource     data-share 声明
├── ICON.PNG / ICON_256.PNG
└── ui/                 ui/config（桌面入口指向 53319）+ ui/images
```

生命周期脚本 `cmd/main {start|stop|status|restart}`：首次 start 生成 `config.toml` 并用 `bin/tc daemon --config` 拉起；PID、日志、identity、queue、received 全在应用数据目录。

## 迁移（从手工部署切到 fpk）

1. **端口冲突**：先停掉现有手工 `tc daemon`（占着 53318/53319），否则 fpk 启动会因端口占用失败。
2. **沿用旧身份**：把旧 `identity/`（含 `identity.key`/`identity.cer`/`trust.jsonl`）放到 `应用数据目录/.identity-import/`，首次启动会自动导入并把该目录改名留档，延续原 NAS 指纹、无需重新配对。私钥不进包。
3. **接收落点**：默认应用数据目录 `received/`，按需改 `config.toml`。

## 与源码的关系

`fpk/` 只放打包脚本与生命周期模板，不改 Rust 源码；`build-fpk.py` 负责组装 fpk。
