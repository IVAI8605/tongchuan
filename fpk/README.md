# 通传 · NAS fpk 包（fnOS 应用）

把通传打包成飞牛 OS（fnOS）可安装的 `.fpk`，用应用中心管理（安装/启停/升级），替换现在「手工 systemd + 裸二进制」的部署方式。

## 版本

- fpk 版本：**1.0.0**（用户 2026-11-23 定）。
- 内含的 `tc` 二进制取自当前源码 release 构建（v1.2.1）。

## 构建

```bash
# 先出 tc 二进制（release）
cd src && CARGO_TARGET_DIR=$AI_TOOL/build/tongchuan-target cargo build --release -p tongchuan
# 再打 fpk（纯 Python 标准库，无外部依赖）
python3 src/fpk/build-fpk.py
```

产物：`dist/tongchuan-1.0.0-linux-x64.fpk` 与同名 `.sha256`。权威副本归档到 `_外置/产物/tongchuan/release-fpk-v1.0.0/`。

## 安装（飞牛应用中心）

在飞牛 OS「应用中心 → 手动安装」上传 `tongchuan-1.0.0-linux-x64.fpk` 即可。装完在应用列表出现「通传 · 局域网互传」，点开是管理页（默认端口 53319，需带令牌；应用 `cmd/main start` 的输出里会打印带令牌的完整链接）。

## 包结构（照 Magpie/DiskSentry 的 fnOS 打包规范）

```
fpk = gzip(tar)
├── manifest            appname/version/display_name/.../service_port/checksum(=app.tgz 的 md5)
├── app.tgz             bin/tc + ui/config + ui/images/icon_*.png
├── cmd/*               install_init / install_callback / main / uninstall_* / upgrade_*（带可执行位）
├── config/privilege    run-as root（要监听端口、写接收落点）
├── config/resource     data-share 声明
├── ICON.PNG / ICON_256.PNG
└── ui/                 ui/config（桌面入口指向 53319）+ ui/images
```

生命周期脚本 `cmd/main {start|stop|status|restart}`：首次 start 会在应用数据目录生成可编辑的 `config.toml`，用 `bin/tc daemon --config` 拉起守护进程；PID 在数据目录，日志在 `logs/`。

## 迁移注意（从手工部署切到 fpk）

1. **端口冲突**：现有手工的 `tc daemon` 占着 53318/53319。装 fpk 前先停它（`kill <pid>`），否则 fpk 应用启动会因端口占用失败。
2. **信任默认开放（安全功能默认关闭）**：生成的 `config.toml` 里 `trust_mode = "open"`，同网段发现即可传、免配对。要严格指纹配对，改成 `"closed"` 并在受信任名单批准对方。要沿用旧设备身份/信任，把旧 `identity/` 拷进新数据目录即可（fpk 数据目录默认独立、不继承旧 identity）。
3. **接收落点**：默认 `/vol2/1000/好3T/通传接收`（与手工部署一致）。要换就改数据目录里的 `config.toml` 的 `destination`（连同 `allow`），然后 `cmd/main restart`。
4. **卸载不删数据**：`uninstall_callback` 只提示数据位置，身份/信任/已收文件都保留。

## 与源码的关系

`fpk/` 只放打包脚本与生命周期模板，不改 Rust 源码。`cmd/*.sh` 是 fnOS 应用生命周期脚本；`build-fpk.py` 负责组装 fpk。
