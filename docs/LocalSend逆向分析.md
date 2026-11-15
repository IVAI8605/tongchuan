# LocalSend 1.18.2 逆向分析（Windows 装机版）

> 分析对象：`C:\Program Files\LocalSend\localsend_app.exe`
> 分析方式：静态分析（文件版本信息、Flutter AOT 快照字符串、Rust 动态库字符串与常量池），并与上游源码 / 协议文档逐条对照
> 上游仓库：`github.com/localsend/localsend`，tag **v1.18.2**
> 协议文档：`github.com/localsend/protocol`（main 分支，Protocol v2.2）
> 分析日期：2026-11-09

证据等级说明：

- **【实测】**：直接来自本机安装文件（版本号、字节数、哈希、二进制里的字符串/常量）。
- **【源码确认】**：来自上述 tag 的上游源码或协议文档，行号为该 tag 文件内的行号。
- **【推断】**：由前两类证据推导，但没有直接观测（例如未抓包、未真机联调）。

---

## 0. 结论摘要

1. **这版 LocalSend 的协议实现已经从 Dart 迁移到 Rust**：`rust_lib_localsend_app.dll`（约 20 MB）里能看到 `packages/core/src/http/server/v2.rs`、`multicast/mod.rs`、`webrtc/` 等上游源码路径与错误文案，Dart 侧（`data/app.so`）只负责 UI 与落盘策略。【实测】
2. **实际互通协议是 v2.2**：默认 UDP 组播 `224.0.0.167:53317` 发现，HTTPS（RSA-2048 自签证书）做传输，REST 路径全部在 `/api/localsend/v2/*`。【实测+源码确认】
3. **默认强制双向 TLS（mTLS）**：只要没开"链接分享/链接接收"网页，服务端就要求客户端出示证书；但它对"谁签的"不校验，只校验证书本身是否有效，再用指纹固定对端。【源码确认】这一点直接决定自研客户端能不能连上装好的 LocalSend。
4. **v3 / WebRTC 已编译进二进制，但在 GUI 里被硬编码关闭**（`const webRTCEnabled = false`），`/api/localsend/v3/register` 也标注 TODO 未接线。当前不要按 v3 设计自研协议。【实测+源码确认】
5. **多文件夹能力的真实卡点不在"能不能传目录"，而在"目录只是一段带斜杠的文件名"**：
   - 发送端一次只能选 **1 个目录**（文件可以多选），多个目录要重复添加；
   - 目录结构靠 `fileName = "目录名/相对路径"` 隐式表达，协议里**没有目录/树对象**；
   - 同名目录会被合并到同一个前缀下，用户无法指定根目录名；
   - 空目录不传；
   - 接收端（GUI）会按路径建目录，且有 `..` / 绝对路径 / 保留名的三层防护；**但官方 CLI 接收端会把路径折叠成最后一个文件名，目录结构全丢**。
6. **没有断点续传、没有协议级进度、没有持久发送队列**：单会话（第二个请求直接 409）、文件级重试仅限 SHA-256 不匹配（最多 3 次）、接收历史最多 30 条且只记接收。

---

## 1. 版本与打包结构【实测】

| 项 | 值 |
|---|---|
| 产品版本 / 文件版本 | `1.18.2+64` |
| `localsend_app.exe` | 220,896 字节，SHA-256 `CAC245D2F7C291195B4E9281F8828A3B32E50898EC1E1CE41F0817EA54E5AFAC` |
| `data\app.so`（Flutter AOT 快照） | 16,073,648 字节 |
| `rust_lib_localsend_app.dll`（协议核心） | 20,265,184 字节 |
| `rhttp.dll` | 3,898,368 字节 |
| `data\flutter_assets\assets\` | `CHANGELOG.md`、`web\`（浏览器分享页 `index.html` + `main.js`）等 |

关键字符串证据（从 `rust_lib_localsend_app.dll` 提取）：

```
packages\core\src\http\server\v2.rs
packages\core\src\http\server\v3.rs
packages\core\src\http\server\web.rs
packages\core\src\http\server\common\save.rs
packages\core\src\http\server\common\client_cert_verifier.rs
packages\core\src\multicast\mod.rs
packages\core\src\discovery\mod.rs
packages\core\src\webrtc\signaling.rs
packages\localsend_isolates\rust\src\api\server.rs
/apps/localsend/...（IDE 编译期绝对路径，说明官方构建环境）
webrtc-srtp
Data channel opened. Exchanging nonce...
Blocked by another session
Checksum mismatch
```

这些路径与上游 tag v1.18.2 的文件树一一对应，因此下文用上游源码解释二进制行为是有依据的。

---

## 2. 发现与配对

### 2.1 组播参数【源码确认】

`packages/core/src/multicast/mod.rs`：

- L28：`DEFAULT_MULTICAST_GROUP = 224.0.0.167`（注释说明选 `224.0.0.0/24` 是因为部分 Android 只接受这个网段）
- L35-36：IPv6 组播 `ff12::fd3a:e420`（链路本地、transient）
- L39：`DEFAULT_PORT = 53317`，组播端口和 HTTP 端口默认相同
- L45-49：启动时连发 3 次公告，间隔 `100ms / 500ms / 2000ms`（防单包丢失、给刚入网设备留时间）
- L53：接收缓冲 64 KiB
- 注释还说明：收到公告的设备**用 HTTP 回复注册请求**，所以 UDP 消息只有"公告"，没有"应答"

### 2.2 公告报文【源码确认】

`packages/core/src/model/discovery.rs` L101-107：`MulticastMessageV2`，camelCase，8 个字段：

```json
{
  "alias": "DESKTOP-XXX",
  "version": "2.2",
  "deviceModel": "Windows",
  "deviceType": "desktop",
  "fingerprint": "A1B2...（证书 SHA-256 大写十六进制）",
  "port": 53317,
  "protocol": "https",
  "download": true
}
```

> 注意：官方协议文档 v2.2 的公告里还有 `announce: true`，但 1.18.2 的 Rust 结构体里**没有这个字段**，靠 serde 忽略未知字段来兼容。【源码确认】
> 自研实现建议带上 `announce: true`，老版本（Dart 实现）可能依赖它；新版本会忽略。

### 2.3 注册握手【源码确认】

- 收到公告后，对本机认为的新设备发起 `POST /api/localsend/v2/register`，body 就是上面的 8 字段（`dto_v2.rs` L15-50）。
- 响应 6 字段：`alias, version, deviceModel, deviceType, fingerprint, download`（`dto_v2.rs` L55-84）。
- `fingerprint` 在 HTTPS 模式下被**忽略**，改用 TLS 客户端证书的 SHA-256（`v2.rs` L20-32 注释 + `receive_controller.dart` L87-89：`event.certFingerprint ?? event.info.fingerprint`）。
- 兜底发现：老式 HTTP 探测 `GET /api/localsend/v1/info` 或 `/v2/info`（`http/server/mod.rs` L624-631），并支持子网扫描，并发 50（`discovery/mod.rs` L31-33）。这条对 NAS 很重要：**组播不通时还能靠扫描发现**。
- 单次注册超时 500 ms（`discovery/mod.rs` L26）。

### 2.4 指纹与证书【源码确认】

`packages/core/src/crypto/cert.rs`：

- L7-56：设备身份 = **RSA-2048** 私钥 + 自签证书；`CN=LocalSend User`，**无 SAN**；有效期用 rcgen 默认（1975–4096），等于永不过期；指纹 = 证书 DER 的 SHA-256，大写十六进制。
- L58-97：校验只看签名、有效期、可选公钥匹配——**不校验证书链/CA**。
- 注释明确说 RSA-2048 是为了和"历史上 Dart 端生成的证书"保持一致。

### 2.5 双向 TLS 的真实规则【源码确认】

- `http/server/mod.rs` L392-394：
  `mandatory_client_auth = app_state.web.is_none() && !app_state.web_upload`
  即：**没开"链接分享/链接接收"网页时，客户端证书是强制的**。默认设置下就是强制。
- `http/server/common/client_cert_verifier.rs` L54-66：任何"本身有效"的客户端证书都接受（日志里 `We trust any certificate that is valid`）；开启网页才把客户端证书改为可选。
- 客户端侧 `http/client/server_cert_verifier.rs` + `send_provider.dart` L93-95：发送时把连接**固定（pin）到目标指纹**，防止同网段其他人应答。

**这一条是自研互通的第一道门槛：没有客户端证书，TLS 握手就过不去。** 自研客户端也必须生成一份自签证书并作为客户端证书出示（不需要被对方签名）。

### 2.6 PIN 与自动接收【源码确认】

- `common/pin.rs` L8-47：接收 PIN 通过 **query 参数 `pin`** 传递；同一 IP 连续失败 3 次后返回 `429 Too many requests`。
- 自动接收（`quickSave`）三档：`off / paired（仅收藏设备）/ on`，默认 `paired`（`persistence_provider.dart` L456-458）。
- 目录传输会让"快速保存"自动关闭（Dart 字符串：`Turned off automatically because there are folders.`）——因为落盘位置需要用户确认。【实测】

---

## 3. 传输协议（v2.2 REST）

### 3.1 路由总表【实测+源码确认】

`http/server/mod.rs` L606-664 的 match 就是完整路由表：

| 方法 | 路径 | 用途 | 备注 |
|---|---|---|---|
| GET | `/` | 浏览器接收/上传页 | 只有开启链接功能时才存在 |
| GET | `/i18n.json` | 网页多语言 | 同上 |
| GET | `/api/localsend/v1/info` | 老客户端探测 | 返回 v2 结构的 info |
| GET | `/api/localsend/v2/info` | 设备信息 | 与 register 响应同构 |
| POST | `/api/localsend/v2/register` | 注册/发现确认 | 8 字段请求，6 字段响应 |
| POST | `/api/localsend/v2/prepare-upload` | 发起发送（元数据） | 单会话，冲突 409 |
| POST | `/api/localsend/v2/upload?sessionId=&fileId=&token=` | 上传单个文件原始字节 | 200 / 422 / 500 |
| POST | `/api/localsend/v2/cancel?sessionId=` | 取消会话 | 也接受"未知 sessionId 的待定请求" |
| POST | `/api/localsend/v2/prepare-download` | 反向：浏览器请求下载 | 链接分享 |
| GET | `/api/localsend/v2/download?sessionId=&fileId=` | 反向：取文件 | 需已接受的 sessionId + 同 IP |
| POST | `/api/localsend/v2/show?token=` | 内部：唤起已有窗口 | 单实例机制，非文件传输 |

### 3.2 准备发送（prepare-upload）

请求（按 `dto_v2.rs` L86-97 构造，未抓包）：

```json
{
  "info": { "alias": "...", "version": "2.2", "deviceModel": "...", "deviceType": "desktop",
            "fingerprint": "...", "port": 53317, "protocol": "https", "download": true },
  "files": {
    "6f1c...": {
      "id": "6f1c...",
      "fileName": "照片/2026/a.jpg",
      "size": 123456,
      "fileType": "image",
      "sha256": "....",
      "preview": "data:image/png;base64,...",
      "metadata": { "modified": "2026-11-09T10:00:00Z", "accessed": "2026-11-09T10:00:00Z" }
    }
  }
}
```

响应：

```json
{ "sessionId": "uuid", "files": { "6f1c...": "token-uuid" } }
```

关键返回码【源码确认】：

| 情况 | 状态码 | body |
|---|---|---|
| 接收方已接受（至少 1 个文件） | 200 | 上面的 JSON |
| 接收方全部拒绝 | 403 | `Rejected` |
| 已有活动会话 | **409** | `Blocked by another session` |
| 接收方一个文件都没选 | 204 | 空 |
| 发送方在等待决定期间取消 | 403 | `Cancelled by sender` |

`files` 映射里**只出现被接受的文件**，这是"选择性接收"的协议基础：未出现的 fileId 发送端直接跳过（`send_provider.dart` L592-616）。

### 3.3 上传（upload）

- `POST /api/localsend/v2/upload?sessionId=..&fileId=..&token=..`，body 是**整文件原始字节流**，无 multipart。
- 校验顺序（`v2.rs` L362-453）：sessionId 存在 → 来源 IP 与建会话时一致 → fileId 存在 → token 匹配 → 该文件状态必须是 `Pending`（同文件不允许并发/重复上传）。
- 成功后：200 空 body；写盘失败：500；**SHA-256 不匹配：422 `Checksum mismatch`**。
- 失败后的状态机（`v2.rs` L646-670）：
  - `MAX_UPLOAD_ATTEMPTS = 3`；
  - 仅"校验和不匹配"会把文件重置回 `Pending` 允许重传（同 token）；
  - 其它失败直接 `Failed`，会话继续等其它文件。
- 客户端并发：同一任务**最多 2 个文件并行**（`upload_isolate.dart` L16-17、L146）。
- 服务端写盘缓冲 512 KiB，上传通道容量 16（`common/save.rs` L11-15），属于流式写，不会把整文件读进内存。

### 3.4 文件元数据字段（`model/transfer.rs` L102-120）

| 字段 | 必填 | 说明 |
|---|---|---|
| `id` | 是 | 发送端生成的 UUID，会话内唯一 |
| `fileName` | 是 | **可带 `/` 的相对路径**（多目录就靠这个） |
| `size` | 是 | 字节数，服务端用它校验收到的长度 |
| `fileType` | 是 | `image/video/pdf/text/apk/other`（按扩展名猜） |
| `sha256` | 否 | 开启"生成校验和"时才有（默认开启） |
| `preview` | 否 | 图片缩略图 data URI |
| `metadata.modified/accessed` | 否 | 写入后回填文件时间 |

### 3.5 传输模型与进度

- **整文件流，不分片**（没有 chunk 索引、没有 Range）。
- **断点续传：没有**。协议里没有 offset/Range 字段；中断后只能整个文件重传。【源码确认：全部 DTO 与路由均无相关字段/头】
- **进度上报：没有协议级事件**。发送端在本地按已写字节数算百分比（`http_upload.dart` L53-54 的 `RsUploadEvent_Progress`），接收端也是本地统计后更新 UI。没有 SSE/WebSocket 进度通道（WebSocket 只用于默认关闭的 WebRTC 信令）。
- 传输全程 HTTPS（默认），自签证书 + 指纹固定；可选关加密走明文 HTTP（`https=false`）。

### 3.6 反向传输（链接分享 / 浏览器）

- `POST /api/localsend/v2/prepare-download` 建下载会话；`GET /api/localsend/v2/download?sessionId=&fileId=` 取文件（`web.rs` L253-370）。
- 会话校验 `session.accepted && session.ip == client_info.ip`，即**同一 IP 才能续用会话**（防会话劫持）。
- 内置网页 `assets/web/main.js` 就直接调用这两个接口，说明网页和原生走同一套 v2 协议。【实测】

---

## 4. 多文件 / 多目录：到底卡在哪（核心）

### 4.1 发送端能力

| 能力 | 结论 | 证据 |
|---|---|---|
| 一次多选文件 | 支持 | 文件选择器多选（`file_picker.dart` L146-170） |
| 一次多选目录 | **不支持**，一次只能选 1 个目录 | `file_picker.dart` L217-227 `pickDirectoryPath()` 返回单个路径 |
| 多个目录 | 可以"重复添加"实现，但每次是一个独立动作 | 同上 + `AddDirectoryAction` 串行追加 |
| 目录递归 | 支持（`Directory.list(recursive: true)`） | `selected_sending_files_provider.dart` L161 |
| 目录名保留 | 支持：`fileName = "$目录名/$相对路径"` | 同文件 L159-164 |
| 忽略文件 | 支持 `.localsend-ignore` | 同文件 L160-170 |
| 空目录 | **不传**（只枚举文件） | 同文件 L161-162 |
| Android | SAF 目录选择，`content://` 相对路径推断 | `AddAndroidDirectoryAction` L207-236 |

发送端拼名字的原文（`selected_sending_files_provider.dart` L163-164）：

```dart
final innerRelative = p.relative(entity.path, from: directoryPath).replaceAll('\\', '/');
final relative = '$directoryName/$innerRelative';
```

**即：协议里没有"目录"这个对象，只有一条带斜杠的文件名。** 目录树的语义完全靠字符串前缀表达。

### 4.2 接收端落盘（GUI）

`packages/localsend_isolates/lib/src/task/server/file_saver.dart`：

- L160-197 `sanitizeRelativeName`：按分隔符拆分，**逐段**处理：
  - `..` 或绝对路径根 → 直接抛 `Path traversal detected`（不是改写）；
  - `.` 和空段丢弃；
  - 每个剩余段调用 Rust `sanitizeFileName`（非法字符、Windows 保留名、长度）。
- L228-237 第二道闸：拼出的目录必须 `p.isWithin(parentDirectory, dir)`，否则再抛一次。
- L243 `Directory(dir).createSync(recursive: true)`：递归建目录，这就是"目录结构保留"的实现。
- L245-251 同名处理：`a.jpg` → `a (1).jpg` → `a (2).jpg`，**不覆盖、不合并内容**。
- L208-226 Android：SAF 走 `content://`，用 `createMissingDirectoriesAndroid` 逐级建目录。

Rust 侧 `util/filename.rs`（L1-40）定义了三套规则并集：Windows 非法字符 `<>:"/\|?*`、HFS 的 `:`、POSIX 的 `/`；Windows 保留名 `con/prn/aux/nul/com1-9/lpt1-9`；单段最长 255 字节。另有 `sanitize_path`（只取最后一段），**CLI 用的就是它**。

### 4.3 五个真实卡点（这就是要超越的地方）

**卡点 1：目录不是一等公民。**
协议只有 `fileName` 一条字符串。没有"根目录 ID/名称/权限/空目录"的表示法，因此：
- 空目录必然丢；
- 无法表达"两个不同根目录但同名子目录"；
- 无法表达符号链接、文件权限、修改时间的目录级属性。

**卡点 2：根目录名不可控，同名根会合并。**
`fileName` 前缀用的是发送端 `p.basename(directoryPath)`。发送 `X:\项目A\src` 和 `Y:\备份\src` 时，两边都变成 `src/...`，接收端会落进同一个 `src/` 目录，只能靠文件名去重（`(1)`、`(2)`），产生大量改名。接收端也无法"另存为别的根名"。

**卡点 3：一次只能选一个目录，多目录体验割裂。**
多目录 = 多次点"添加文件夹"，每次一个独立动作；选择列表里看不出目录边界，取消也只能逐个文件删；重名目录在发送前不会提示。

**卡点 4：接收端只能按"文件"选择，不能按"目录"选择。**
`prepare-upload` 返回的 token 是**每个文件一个**（`v2.rs` L316-321），UI 也是逐文件勾选（`selectedReceivingFilesProvider`）。想"只要某个子目录"只能手动勾选其中所有文件；且一旦包含目录，`quickSave` 自动关闭，无法无人值守。

**卡点 5：官方 CLI 接收端把目录结构压平。**
`cli/src/util.rs` L95-111：

```rust
pub fn unique_path(dir: &Path, file_name: &str) -> PathBuf {
    let name = filename::sanitize_path(file_name, filename::Rules::current());
    let candidate = dir.join(&name);   // 只剩最后一段
    ...
}
```

所以用官方 `localsend-cli` 跑在 NAS 上收"文件夹"，结果会全部平铺到目标目录，再靠 ` (n)` 去重。**这意味着官方方案在 NAS 场景下根本不能保留目录树**，这正是自研的第一个价值点。

### 4.4 路径穿越防护评价

GUI 接收端做得比较扎实：**逐段**清洗 + 拒绝而非改写 + `isWithin` 二次校验 + 保留名/非法字符处理 + 长度限制。可以认为"能防住经典 `../../`、绝对路径、Windows 保留名"。【源码确认】

仍有边角（【推断】）：
- Windows 超长路径（>260 字符且无 `\\?\` 前缀）深层目录可能创建失败；
- 大小写不敏感文件系统上，`A/` 与 `a/` 会合并；
- 不支持符号链接，若目标目录内已有同名符号链接，写入会跟随链接（取决于 Rust `File::create` 行为），自研应显式 `O_NOFOLLOW` 或先 lstat。

---

## 5. 会话、队列、持久化

| 维度 | 事实 | 证据 |
|---|---|---|
| 并发会话 | **单会话**：第二个 `prepare-upload` 直接 409 | `v2.rs` L256-261 |
| 会话绑定 | sessionId + 发送方 IP 双绑定 | `v2.rs` L387 |
| 会话存储 | 纯内存，进程退出即丢；重开 App 无法续传 | `session.rs`（无落盘） |
| 发送队列 | 内存态，无持久队列；1:n 多设备是多个 session 并行 | `send_provider.dart` |
| 断线重连 | 无协议支持；HTTP 断 → 该文件失败 | 无相关路由/字段 |
| 失败重试 | 仅 SHA-256 不匹配自动重试，最多 3 次（同 token） | `v2.rs` L646/L666、`upload_isolate.dart` L19-23 |
| 用户手动重试 | 以"重新发送整批"实现 | UI 行为 |
| 同名文件 | 接收端加 ` (n)`，不覆盖；重试时复用同一路径避免生成第二份 | `file_saver.dart` L96-116、L245-251 |
| 接收历史 | 最多 **30 条**，仅接收，存本地偏好 | `receive_history_provider.dart` L6/L66 |
| 发送历史 | **无** | 无对应 provider |
| 自动完成 | `autoFinish` 设置项 | `settings_state.dart` L28 |

---

## 6. 平台差异与 NAS 场景

### 6.1 三端共性与差异

- 协议核心是同一份 Rust（`packages/core`），因此**发现/握手/传输语义三端一致**；差异集中在 UI 与落盘。
- Windows：单实例靠 `/api/localsend/v2/show?token=` 唤起已开窗口（`internal.rs`）；托盘最小化。
- Android：SAF + MediaStore，图片/视频可"保存到相册"；`content://` 路径无法直接 `File` 打开，走文件描述符。
- Linux 桌面：仍是 Flutter GUI，需要 X11/Wayland；**另有一个独立的 Rust CLI**（`cli/`，二进制名 `localsend-cli`），纯终端运行。

### 6.2 NAS（Debian 12，无 GUI）会遇到什么

| 问题 | 结论 | 依据 |
|---|---|---|
| 有没有无头方案 | 上游有 Rust CLI，但它是**交互式 TUI**（Y/N/P 按键、设备列表、文件选择器），非交互 SSH 下不可用 | `cli/src/main.rs` L35-47 |
| 组播 | 需要交换机 IGMP snooping 放行；失败不致命，会退回子网 HTTP 扫描（较慢） | `multicast/mod.rs` 注释、`discovery/mod.rs` L31-33 |
| 是否依赖 mDNS/Bonjour | **不依赖**，自己发 UDP 组播，NAS 不需要 avahi | 全文无 mDNS 代码 |
| 防火墙 | 需要放行 TCP 53317 与 UDP 53317 | 默认端口 |
| 非 root | 53317 > 1024，普通用户可监听；但保存到 `/vol1` 需目录权限 | 常识 |
| 加密 | CLI 强制 HTTPS + mTLS（`identity.tls_config()`），与 GUI 互认 | `cli/src/app/mod.rs` L111-124 |
| 目录结构 | **CLI 会压平**（见 4.3 卡点 5） | `cli/src/util.rs` L95-111 |
| 配置 | `config.toml` 仅 `alias/port/destination` 三个键，环境变量 `LOCALSEND_*` 覆盖 | `cli/src/storage/config.rs` |
| 端口冲突 | 若 NAS 上已装 LocalSend，再跑自研服务会抢占 53317，必须可配置 | `--port` 参数 |

**结论：官方在 NAS 上"能收但不能保结构、不能无人值守、不能脚本化"。这三条是自研最直接的切入点。**

---

## 7. 协议演进与兼容

| 版本 | 内容 | 1.18.2 的处理 |
|---|---|---|
| v1 | `GET /api/localsend/v1/info` | 保留，返回 v2 结构（`server/mod.rs` L624-631） |
| v2.0 | `info / prepare-upload / upload / cancel` | 主力路径 |
| v2.1/2.2 | 新增 `register`、`download` API、`download` 字段、网页分享 | 完整实现，`PROTOCOL_VERSION_V2 = "2.2"`（`model/discovery.rs` L4） |
| v3 | `/api/localsend/v3/nonce`、`/v3/register`、WebRTC 数据通道 + 信令 `wss://public.localsend.org/v1/ws` + STUN `stun.localsend.org:3478` | 已编译，但 GUI `webRTCEnabled = false`；`v3::register` 标注 TODO 未接线（`http/server/v3.rs`） |

跨版本互传：
- 1.18.2 ↔ 1.17 及更早：可行，老客户端探测 v1/info 能被正确回答。【源码确认】
- v2 的 DTO 都用 serde 默认策略，**未知字段会被忽略**，因此新版多出来的 `download` 字段不会打断老客户端；反向也兼容。【源码确认】
- 不要指望 v3：当前是死代码，等官方启用后再跟进。

---

## 8. 自研方案建议

### 8.1 先定路线：复用 v2.2 做"互通层"，自研扩展做"多文件夹层"

建议**双栈**：

```
┌─────────────────────────────────────────┐
│ 自研协议 v1（多文件夹、断点续传、事件）      │  ← 自家 Android / Windows / NAS 之间用
├─────────────────────────────────────────┤
│ LocalSend v2.2 兼容层（发现/注册/HTTPS/mTLS）│  ← 与装了 LocalSend 的设备互传
└─────────────────────────────────────────┘
```

理由：
- 与装机 LocalSend 互通必须**逐字节按 v2.2 实现**（组播报文、注册、prepare-upload、upload、422 重试、409 单会话、mTLS、指纹固定）。自己另发明一套就只能自家互传，丧失"发给别人手机"的卖点。
- 但 v2.2 的目录表达太弱，不应把它当主协议。自研协议可以在兼容层之上做增强。

### 8.2 兼容层必须实现的清单（否则连不上）

1. UDP 组播：`224.0.0.167:53317`（可选 IPv6 `ff12::fd3a:e420`），公告 3 连发，带 `announce:true` 以求最大兼容。
2. 自签 RSA-2048 证书（`CN=LocalSend User`，无 SAN），**并作为客户端证书出示**（默认强制 mTLS）。
3. 服务端证书**按指纹固定**，不要走系统 CA 校验（自签必然失败）。
4. 注册：`POST /api/localsend/v2/register`，响应里的 fingerprint 在 HTTPS 下以 TLS 证书为准。
5. 发送：`prepare-upload` → 按返回的 token 逐文件 `POST /upload`（body 原始字节 + 正确 Content-Length）→ 失败区分 422（可重试，≤3 次）与 500（不重试）。
6. 接收：实现 `prepare-upload` 的决策（可全拒 403、可全收 200、可部分收）、单会话 409、文件状态机、`cancel`。
7. 可选：`pin` query、`prepare-download/download`（给别人网页下载）、`/api/localsend/v1/info`。

### 8.3 多文件夹在协议层怎么设计

**核心思路：把"目录"提升为一等公民，但仍保留 `fileName` 路径前缀以兼容 LocalSend。**

建议清单（自研协议 v1）：

```json
{
  "txnId": "uuid",
  "roots": [
    { "id": "r1", "name": "项目A", "emptyDirs": ["docs/img", "build"] },
    { "id": "r2", "name": "照片",   "emptyDirs": [] }
  ],
  "files": [
    { "id": "f1", "rootId": "r1", "relPath": "src/main.rs", "size": 1234,
      "sha256": "...", "mtime": "...", "mode": 420 }
  ]
}
```

要点：

1. **目录树显式化**：`roots[] + relPath`，可表达多个根、空目录、目录级属性；解决卡点 1、2。
2. **根名可改**：接收端可把 `r1` 映射到任意本地目录（"选择性接收"= 用户为每个 root 选"接收/跳过/另存为"），解决卡点 3、4。
3. **传输仍按文件流**：每个文件一个独立请求（可复用 v2 的 token 思路），并发 2~4；**增加 `Range` 或 chunk manifest 支持断点续传**（v2 没有，这是自研的差异化能力）。
4. **进度与事件**：`GET /events`（SSE）或 WebSocket，推 `file-progress / file-done / txn-done`；比 v2 的"本地估算"体验好，也便于 NAS 侧无 GUI 观察。
5. **可选接收**：先用 manifest 交换意图，再对接受的文件发 token 列表；目录级接受在 manifest 层完成，文件级仍是 token。
6. **路径安全（照抄并加强 LocalSend）**：
   - 拒绝 `..`、绝对路径、盘符、UNC、`\\?\`；
   - 逐段 sanitize（Windows 非法字符 + 保留名 + 255 字节/段）；
   - `p.isWithin` 二次校验；
   - 目标目录 lstat，拒绝跟随符号链接；
   - Windows 路径超 260 时加 `\\?\` 前缀或拒绝并提示；
   - 大小写不敏感文件系统做冲突检测（`A/` 与 `a/`）。
7. **同名策略可配置**：`rename（默认）/ overwrite / skip / merge`；LocalSend 只有 `rename`。

### 8.4 与 LocalSend 兼容时的目录映射

当自研端把多文件夹发给**原版 LocalSend** 时，只能降级：
- 每个 root 作为一个前缀：`fileName = "{rootName}/{relPath}"`；
- 同名 root 必须提前改名（自研侧自动加唯一后缀，如 `项目A-2f3c/...`），否则对方会合并；
- 空目录无法表达，直接跳过并在 UI 说明；
- 接收端（原版）会逐段 sanitize 并建目录，行为可预期。

### 8.5 Android 端怎么做

- **发现**：持有 `MulticastLock`（否则部分机型收不到组播），同时提供"手动输入 IP:端口"兜底（对应 v1/v2 info 探测）。
- **接收**：前台服务 + 常驻通知；落盘优先 SAF / MediaStore（`content://`），目录传输用 `createMissingDirectoriesAndroid` 等价实现；不要直接写 `/sdcard` 绝对路径（Android 10+ 分区存储会失败）。
- **发送**：SAF 目录选择后，用 `DocumentFile` 递归枚举并计算相对路径；大文件用文件描述符流式读取，避免整文件进内存。
- **兼容层**：证书与私钥存 Keystore；指纹固定表持久化（收藏设备）。
- **注意**：Android 端做服务端时要处理息屏/省电杀进程，v2 的单会话模型下断线就意味着整批失败。

### 8.6 无 GUI 的 Debian 服务器怎么做

- **形态**：自研做**无头守护进程**（Rust 最优，可直接依赖上游 `localsend` crate 的 `multicast` / `http::server::v2` / `crypto` 模块，省掉重写协议），用 systemd 托管；**不要用官方 CLI**（交互式 TUI、且压平目录）。
- **配置**：`/etc/自研/config.toml` 或环境变量：`alias / port / destination / roots / disabled_roots / tls_cert_dir`。
- **目录映射**：`roots` 支持 `name -> /data/...` 白名单；默认只允许写入白名单目录，防止接收端被当成任意写盘入口（LocalSend 只有"目标目录"一个，安全性不足）。
- **权限**：以专用用户运行，目标目录授予 ACL；**不要以 root 运行**（上游也只需普通用户）。注意该平台上应用目录属主敏感，写操作只落在数据卷。
- **网络**：监听 `0.0.0.0:53317`；组播需要 IGMP；若 NAS 在 Docker 里，必须用 host 网络，否则组播与端口映射都会出问题。
- **端口冲突**：与已装 LocalSend 冲突时，自研端口可配，并在启动时探测占用。
- **可观测**：日志写 journald；提供 `GET /status` 返回当前会话、进度、最近 N 条记录（原版 CLI 只有 TUI）。
- **断点续传**：NAS 场景最需要（大文件 + 家庭网络抖动），自研协议必须支持；与 LocalSend 互通时退化为整文件重传。

---

## 9. 没能查清的部分 / 后续需要的信息

1. **没有抓包**：所有报文样例是按同版本 DTO 构造的，未做线上验证。若要写"请求/响应样例"进正式文档，需要在第二台设备上抓 `tcpdump`/Wireshark 核对（尤其 TLS 下的字段顺序不影响 JSON，但 header 细节如 `Content-Type`、`Expect: 100-continue` 未验证）。【待验证】
2. **mTLS 的具体握手细节**：服务端是否请求 `CertificateRequest` 的可接受 CA 列表（`root_hint_subjects` 加入了自身证书）未实测；部分 TLS 库在"客户端证书不受信任"时行为不同，自研需实测。【推断/待验证】
3. **Windows 安装形态**：本次只读了 `Program Files` 下的文件与版本信息，未确认是 MSIX 还是传统安装包（注册表卸载项未取到）。不影响协议结论。【待验证】
4. **超长路径 / 权限失败 / 磁盘满**的具体错误码与 UI 表现未验证；源码只能看出"失败即 500/文件 Failed"。【推断】
5. **v3 / WebRTC 的最终形态**：当前 GUI 关闭、`v3::register` TODO；等官方启用后再评估。若要用，需要信令服务器（默认 `wss://public.localsend.org/v1/ws`），NAS 场景还要考虑 STUN/TURN。【待验证】
6. **Android/iOS 端版本未知**：本次只分析了 Windows 装机版。手机端若是老版本（v1/v2.0），字段兼容性需实测。【待验证】
7. **第二台设备联调**：需要至少一台手机或另一台 PC，验证：① 自研客户端带自签证书能否通过 mTLS；② 目录传输在原版接收端的落盘结果；③ 原版发送端把目录发给我们时的 `fileName` 实际形态（是否总带根目录名、是否用 `/`）。【待验证，必须真机】

---

## 附录 A：上游源码索引（tag v1.18.2）

| 主题 | 文件 | 关键行 |
|---|---|---|
| 组播常量 | `packages/core/src/multicast/mod.rs` | 28-49 |
| 协议版本 / 公告 DTO | `packages/core/src/model/discovery.rs` | 4、101-130 |
| v2 DTO | `packages/core/src/http/dto_v2.rs` | 15-116 |
| 文件 DTO | `packages/core/src/model/transfer.rs` | 102-140 |
| 路由表 | `packages/core/src/http/server/mod.rs` | 606-664、392-394 |
| 准备上传 / 上传 / 取消 | `packages/core/src/http/server/v2.rs` | 231-360、362-453、455-520、646-670 |
| 写盘与校验 | `packages/core/src/http/server/common/save.rs` | 11-15、1-120 |
| 文件名清洗 | `packages/core/src/util/filename.rs` | 1-120 |
| 证书与指纹 | `packages/core/src/crypto/cert.rs` | 7-56、99-104 |
| 客户端证书校验 | `packages/core/src/http/server/common/client_cert_verifier.rs` | 11-66 |
| PIN | `packages/core/src/http/server/common/pin.rs` | 8-47 |
| 保存策略 | `packages/localsend_isolates/lib/src/task/server/file_saver.dart` | 160-252 |
| 上传并发/重试 | `packages/localsend_isolates/lib/src/isolate/child/upload_isolate.dart` | 16-23、146 |
| 目录发送 | `app/lib/provider/selection/selected_sending_files_provider.dart` | 149-170、207-236 |
| 目录选择器 | `app/lib/util/native/file_picker.dart` | 217-227 |
| 默认设置 | `app/lib/provider/persistence_provider.dart` | 340-341、396-406、456-458、493-495 |
| WebRTC 开关 | `app/lib/provider/network/webrtc/signaling_provider.dart` | 21-22、53-54 |
| CLI 平铺路径 | `cli/src/util.rs` | 95-111 |
| CLI 配置 | `cli/src/storage/config.rs` | 11-34 |
| 接收历史 | `app/lib/provider/receive_history_provider.dart` | 6、66 |

## 附录 B：二进制里的关键字符串（实测）

```
224.0.0.167 / 53317（Dart 设置默认值里可见）
/api/localsend/v2/prepare-upload
/api/localsend/v2/upload
/api/localsend/v2/cancel
/api/localsend/v2/prepare-download
/api/localsend/v2/download
/api/localsend/v2/register
/api/localsend/v1/info
/api/localsend/v3/nonce
/api/localsend/v3/register
Blocked by another session
Checksum mismatch
Rejected
Path traversal detected
con prn aux nul com1..com9 lpt1..lpt9        （Windows 保留名清单）
webrtc-srtp
Data channel opened. Exchanging nonce...
Sent offer to ... / Sent answer to ...
```
