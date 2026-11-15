plugins {
    id("com.android.application")
}

// 版本号唯一来源是根目录 Cargo.toml 的 workspace.package.version。
// 在配置阶段读一次，Android 侧不再手写一份；解析失败就直接让构建失败，
// 避免悄悄打出与 tag 不一致的包。
val cargoToml = rootProject.projectDir.parentFile.resolve("Cargo.toml")
val cargoVersion = Regex("(?m)^version\\s*=\\s*\"([0-9]+\\.[0-9]+\\.[0-9]+)\"")
    .find(cargoToml.readText())?.groupValues?.get(1)
    ?: throw GradleException("无法从 ${cargoToml.path} 读取版本号")
val (verMajor, verMinor, verPatch) = cargoVersion.split(".").map { it.toInt() }

android {
    namespace = "com.tongchuan"
    compileSdk = 35
    // AGP 8.7.3 默认要 34.0.0，本机只装了 36.0.0，显式指定避免去网上下载
    buildToolsVersion = "36.0.0"

    defaultConfig {
        applicationId = "com.tongchuan"
        minSdk = 26
        targetSdk = 35
        versionCode = verMajor * 10000 + verMinor * 100 + verPatch
        versionName = cargoVersion
        // 只出 arm64：自用机型和 NAS 都是 64 位，少一个 abi 就少一半体积
        ndk { abiFilters += "arm64-v8a" }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // 自用侧载，不做正式签名；复用调试签名省掉一套密钥管理
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    // Rust 产物由 scripts/android/build-android.ps1 拷进来，这里只声明目录
    sourceSets["main"].jniLibs.srcDir("src/main/jniLibs")

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    lint {
        // 自用工具，不做上架检查；只关心能不能装、能不能跑
        abortOnError = false
        // 关掉打包前的 lint：它会去网上拉 lint-gradle，离线环境直接构建失败
        checkReleaseBuilds = false
    }
}

dependencies {
    // v0.20 二维码：只解码/编码，不带动整套相机 UI，预览由 ScanActivity 用 Camera2 自己做。
    // 选 zxing core 而不是 zxing-android-embedded：后者会额外带一套 Activity 与资源。
    implementation("com.google.zxing:core:3.5.3")
}
