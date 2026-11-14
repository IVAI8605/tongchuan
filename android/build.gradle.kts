// AGP 版本跟本机已缓存的依赖保持一致（8.7.3），避免为了编译再去网上拉一套新的工具链。
plugins {
    id("com.android.application") version "8.7.3" apply false
}
