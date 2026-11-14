> **已废止（2026-11-12 00:58）**：本文仅作历史留档，不再维护。
> 原因：发布/升级步骤已并入 [服务器端-维护手册.md](../服务器端-维护手册.md) §4、§5，且本文未覆盖服务器端部署。
> 现行文档：[服务器端-维护手册.md](../服务器端-维护手册.md)

# 通传 · 发布流程

> 制定时间：2026-11-10 17:55
> 对齐对象：查重助手（`Verify查重助手/AGENTS.md` 的「版本与发布」）保持同一套口径：版本号单一来源、
> 构建脚本统一入口、门禁全绿才发、tag 与 Release 同名、产物文件名带版本号、发布后读回核对。
> **不引入 CI**，所有步骤本机手动执行。

## 一、版本号来源

| 位置 | 作用 | 是否需手改 |
|---|---|---|
| 根 `Cargo.toml` 的 `workspace.package.version` | **唯一来源**；`tc version`、Android、NSIS 都从它派生 | 是，发版第一步只改这里 |
| `android/app/build.gradle.kts` | 配置阶段读根 `Cargo.toml`，派生 `versionName` 与 `versionCode`（x\*10000+y\*100+z） | 否，自动派生；读不到直接构建失败 |
| `scripts/windows/installer.nsi` | 只认 `-DVERSION` / `-DTC_BUILD`，由 `make-installer.ps1` 从 `Cargo.toml` 取版本后传入 | 否，自动派生；缺参编译失败 |
| `CHANGELOG.md` | 更新日志单源；Release 说明从这里截取 | 是，新增版本段落 |
| git tag / GitHub Release 标题 | `vX.Y.Z` | 与上面一致 |

三处已统一为 `0.4.0`，且只保留 `Cargo.toml` 一处真源；Android 与 NSIS 都不再手写版本号。

## 二、构建

两个产物各走一个脚本，不在文档里重抄命令细节：

```powershell
# Windows 可执行：tc.exe 落到 ASCII 构建目录（路径见 .cargo/config.toml）
& pwsh -NoProfile -File scripts/verify.ps1      # 该脚本第一步就是构建；也可单独 cargo build --release
# Android APK：先交叉编译 tc-jni，再交给 Gradle 打包（默认 --offline）
& pwsh -NoProfile -File scripts/android/build-apk.ps1
```

- 构建产物目录默认是项目上一级的 `../tongchuan-target`（相对路径，可用 `CARGO_TARGET_DIR` 覆盖）。
  放在项目外是为了避开中文目录名：`ring` 的 mingw `ar` 处理不了中文路径。
- 目录名必须保持纯 ASCII：实测中文目录会让 Windows 构建直接失败，所以统一放仓库外。
- 仓库里已没有任何本机绝对路径：SDK / NDK / JDK 按 `ANDROID_HOME`、`ANDROID_NDK_HOME`、`JAVA_HOME`
  探测，都没有时退回工具的默认安装位置；换机器基本不用改文件。
- NSIS 安装包：`& pwsh -NoProfile -File scripts/windows/make-installer.ps1`，本机未装 `makensis` 时
  会明确报错，也可加 `-DryRun` 只看它算出的版本号与命令。`installer.nsi` 不再接受写死的版本号。

## 三、验证（发版门禁）

1. `scripts/verify.ps1` 必须 `TOTAL PASS`：BUILD / TEST / CLI / MANAGE / SCRIPTS / VERSION / HIST 逐项 PASS。
   只回传 PASS/FAIL 摘要，原始日志留在 `.tmp\通传-验证\<时间戳>\` 下，不进对话。
2. 按风险分档决定真机范围（依据 `版本规划.md` 与 `release-verify-playbook`）：
   - A 档（碰安装/升级/权限/设备协议/数据目录）：Windows 安装脚本与 Android 接收都要真机跑通。
   - B 档（只碰接口字段与界面布局）：管理页冒烟即可。
   - C 档（纯代码、单测、本机回环）：门禁全绿即可发布。
3. 结论只允许 `PASS` / `FAIL` / `N-A` / `BLOCKED`。`FAIL` 与 `BLOCKED` 一律不发版。
4. 因环境被占用而没跑成的项（例如发现端口被别的 `tc.exe daemon` 占着）记为 `BLOCKED` 并写明原因，
   不许改成 PASS，也不许直接发版。

## 四、产物命名与附件

| 产物 | 源文件 | Release 附件名 |
|---|---|---|
| Windows 便携包 | `tc.exe` + `scripts/windows/*.ps1` + `config.example.toml` + `assets/tongchuan.ico` | `tongchuan-v<版本>-win-x64.zip` |
| Windows 安装包（可选） | NSIS 输出 `通传-<版本>-setup.exe` | `tongchuan-v<版本>-setup.exe` |
| Android APK | `android/app/build/outputs/apk/release/app-release.apk` | `tongchuan-v<版本>-arm64.apk` |

- **文件名必须带版本号**，不要提交无版本号的 `app-release.apk` 或 `tc.exe` 作为发布件。
- 两个产物都要在 Release 说明里附 **SHA256 与 MD5**，取自实际要上传的那份文件。
- 仓库本身不存二进制：`.gitignore` 已排除 `tc.exe`、`*.apk`、`jniLibs/*.so`，产物只走 Release 附件。

## 五、发布顺序

```
门禁全绿 → 构建双产物 → 真机验证 → 提交 → tag vX.Y.Z → push → GitHub Release（附产物与哈希）
```

- 提交信息用 Conventional Commits：`feat:` / `fix:` / `docs:` / `refactor:` / `chore:`。
- tag 与 Release 标题同名，例如 `v0.5.0`；Release 说明从 `CHANGELOG.md` 对应段落截取，不另写一份。
- 真机验证不通过就先修，不发布；重装时保留 `data/`（证书、日志、队列、已收文件）。
- 全程不删除任何文件；需要清理时先列清单问用户。

## 六、发布后读回

发布不是「命令返回 0」就结束，必须回读核对：

```powershell
# 1) 附件是否真的挂上、名字是否带版本号
gh release view v<版本> --repo <owner>/<repo> --json tagName,name,assets
# 2) 远端 tag 指向的提交是否就是本地要发的那个
git rev-parse v<版本>^{commit}
git ls-remote --tags origin "refs/tags/v<版本>*"
# 3) 下载回来算哈希，与 Release 说明里的值逐位比对
gh release download v<版本> --repo <owner>/<repo> --dir "$env:TEMP\tc-release-check"
Get-FileHash "$env:TEMP\tc-release-check\*" -Algorithm SHA256
```

三项一致才算发布完成；不一致时先查是不是构建完又改了代码（产物与 tag 必须同一份源）。

## 七、不要做的事

- 不引入 CI（除非用户后续明确要求）。
- 不用 `git push --force`，不改写已发布的 tag。
- 不把 `config.toml`、`data/identity/`、日志、APK 提交进仓库。
- 不在发版后顺手删文件或「整理」历史；收尾核对全程只读。
- **不把 `v0.4.0` 当发布点**，见第八节。

## 八、仓库现状与既定约定（2026-11-10 18:05）

- **tag `v0.4.0` 不作发布点**：它指向的是「收纳既有实现的初始基线」提交，里面已经含了 v0.5 在研代码
  （发送端历史与重传）。等 Android 0.4 的通知修复真机验收通过后，再另派步骤重新打 tag；
  在此之前不要用它创建 GitHub Release。
- **clone 后先跑构建脚本**：`android/app/src/main/jniLibs/arm64-v8a/libtc_jni.so` 是可重新构建的产物，
  按约定不入库。新机器 clone 后必须先执行 `scripts/android/build-android.ps1`（或直接跑
  `scripts/android/build-apk.ps1`，它会顺带调用），否则 Gradle 打出的 APK 缺原生库，装上会直接崩。
- **License**：MIT，全文见仓库根 `LICENSE`；仓库公开。
- **可移植性靠环境变量**：`ANDROID_HOME` / `ANDROID_SDK_ROOT`、`ANDROID_NDK_HOME`、`JAVA_HOME`、
  `CARGO_TARGET_DIR` 都能覆盖脚本里的自动探测；`cargo`、`java` 也可走 PATH。
  唯一前提是产物目录为纯 ASCII：默认的 `../tongchuan-target` 只要 clone 位置的上一级目录是 ASCII 就成立，
  否则请显式设置 `CARGO_TARGET_DIR` 到 ASCII 路径。
