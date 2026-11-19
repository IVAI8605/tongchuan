# 第三方组件声明（通传 Android APK）

本文件随 `通传-1.0.0.apk` 一起发布。APK 内含下列第三方组件；各组件保留其原始许可证，
版权归各自作者所有。本项目自身代码见仓库根目录 `LICENSE`（MIT）。

## Java / Kotlin（打进 classes.dex）

| 组件 | 版本 | 许可证 | 来源 |
|---|---|---|---|
| ZXing Core | 3.5.3 | Apache-2.0 | https://github.com/zxing/zxing |
| AndroidX Compose（ui / ui-graphics / material3 / animation / foundation 等，BOM 2024.12.01） | 1.7.x / 1.3.1 | Apache-2.0 | https://developer.android.com/jetpack/compose |
| AndroidX Activity Compose | 1.9.3 | Apache-2.0 | https://developer.android.com/jetpack/androidx/releases/activity |
| AndroidX Lifecycle Runtime Compose | 2.8.3 | Apache-2.0 | https://developer.android.com/jetpack/androidx/releases/lifecycle |
| AndroidX Core KTX | 1.13.1 | Apache-2.0 | https://developer.android.com/jetpack/androidx/releases/core |

## Rust（编进 libtc_jni.so）

| 组件 | 版本 | 许可证 | 来源 |
|---|---|---|---|
| ring | 0.17.14 | ISC / OpenSSL（双许可） | https://github.com/briansmith/ring |
| rustls / rustls-webpki / rustls-pki-types | 0.23.46 / 0.103.16 / 1.15.1 | MIT / ISC / Apache-2.0 | https://github.com/rustls |
| blake3 | 1.8.7 | MIT / Apache-2.0（双许可） | https://github.com/BLAKE3-team/BLAKE3 |
| rcgen | 0.13.2 | MIT / Apache-2.0（双许可） | https://github.com/rustls/rcgen |
| serde / serde_json / serde_derive | 1.0.229 / 1.0.154 | MIT / Apache-2.0 | https://github.com/serde-rs |
| qrcode | 0.14.1 | MIT | https://github.com/wookey-project/rust_qr |
| base64 / time / toml / log / thiserror / jni / libc 等 | 见 `Cargo.lock` | MIT / Apache-2.0 / ISC | crates.io |

完整依赖树与精确版本见仓库根 `Cargo.lock` 与 `android/gradle/libs.versions.toml`。
上述组件的许可证文本可从各自来源获取；本项目不修改其源码。
