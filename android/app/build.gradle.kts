import java.util.Properties
import java.io.FileInputStream
import java.nio.charset.StandardCharsets

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
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

    // 正式签名：环境变量 TC_KEYSTORE_PROPS 指向 keystore.properties（含 storeFile/storePassword/keyAlias/keyPassword）时启用，
    // 否则退回 debug 签名（开发包）。密钥与密码都不入库、不进日志。
    val ksProps = System.getenv("TC_KEYSTORE_PROPS")?.let { p ->
        runCatching {
            Properties().apply { FileInputStream(p).reader(StandardCharsets.UTF_8).use { load(it) } }
        }.getOrNull()
    }
    signingConfigs {
        if (ksProps != null) {
            create("release") {
                storeFile = file(ksProps.getProperty("storeFile"))
                storePassword = ksProps.getProperty("storePassword")
                keyAlias = ksProps.getProperty("keyAlias")
                keyPassword = ksProps.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // 设了 TC_KEYSTORE_PROPS 就用正式密钥签，否则退回 debug（开发/自测包）
            signingConfig = if (ksProps != null) signingConfigs.getByName("release") else signingConfigs.getByName("debug")
        }
    }

    // Rust 产物由 scripts/android/build-android.ps1 拷进来，这里只声明目录
    sourceSets["main"].jniLibs.srcDir("src/main/jniLibs")

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    // 2026-11-19 UI 重构：界面层从 View 体系改为 Jetpack Compose，新代码一律走 Compose
    buildFeatures { compose = true }

    lint {
        // 自用工具，不做上架检查；只关心能不能装、能不能跑
        abortOnError = false
        // 关掉打包前的 lint：它会去网上拉 lint-gradle，离线环境直接构建失败
        checkReleaseBuilds = false
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

dependencies {
    // v0.20 二维码：只解码/编码，不带动整套相机 UI，预览由 ScanActivity 用 Camera2 自己做。
    // 选 zxing core 而不是 zxing-android-embedded：后者会额外带一套 Activity 与资源。
    implementation("com.google.zxing:core:3.5.3")

    // 2026-11-19 UI 重构：Compose + Material 3（版本由 BOM 统一）
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.graphics)
    implementation(libs.compose.ui.tooling.preview)
    implementation(libs.compose.material3)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.core.ktx)
    debugImplementation(libs.compose.ui.tooling)

    testImplementation(libs.junit)
}
