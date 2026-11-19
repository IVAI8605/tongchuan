pluginManagement {
    repositories {
        // dl.google.com 本机不通（实测 SSL_ERROR_SYSCALL），aliyun 镜像优先，官方源作后备
        maven("https://maven.aliyun.com/repository/gradle-plugin")
        maven("https://maven.aliyun.com/repository/google")
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        maven("https://maven.aliyun.com/repository/google")
        maven("https://maven.aliyun.com/repository/central")
        google()
        mavenCentral()
    }
}

rootProject.name = "tongchuan"
include(":app")
