import java.io.FileInputStream
import java.util.Properties

plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

// release 签名：keystore 与口令由 CI 从 GitHub Secrets 还原到 app/android/key.properties（不进仓库）。
// 本地没有该文件时自动退回 debug 签名，`flutter run --release` 仍然可用。
val keystorePropertiesFile = rootProject.file("key.properties")
val keystoreProperties = Properties().apply {
    if (keystorePropertiesFile.exists()) {
        FileInputStream(keystorePropertiesFile).use { load(it) }
    }
}
val hasReleaseKeystore = keystorePropertiesFile.exists()

android {
    namespace = "cn.ynau.yunong"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    // t165：让 .so 以**未压缩、安装时解压**的方式打包（AGP 4+ 默认 useLegacyPackaging=false，
    // 库只留在 APK 里、不落盘；而 Dart 的 DynamicLibrary.open 走 dlopen，只能找到落盘的库）。
    packaging {
        jniLibs {
            useLegacyPackaging = true
            // ABI 收敛（t177）：Flutter 的 universal APK 总会带三套引擎库，而 Rust 核心只有 arm64-v8a。
            // 直接把其它 ABI 的原生库从 APK 里剔掉——这条规则与 splits/abiFilters 都无冲突。
            excludes += listOf("lib/armeabi-v7a/**", "lib/x86/**", "lib/x86_64/**")
        }
    }

    defaultConfig {
        // TODO: Specify your own unique Application ID (https://developer.android.com/studio/build/application-id.html).
        applicationId = "cn.ynau.yunong"
        // 只打包 arm64-v8a：体积可控（单个 .so 约 6 MB），且 2018 年后的真机基本都是 arm64。
        // 需要模拟器联调时可把 x86_64 加进来，并在 scripts/build-android-core.ps1 里多编一个 ABI。
        // ⚠️ 这里不设 ndk.abiFilters（t177 结论）：它和 `--split-per-abi` 生成的 splits.abi 互斥，
        // AGP 会直接报 "Conflicting configuration ... cannot be present when splits abi filters are set"。
        // ABI 收敛统一由命令行负责：`flutter build apk --release --split-per-abi --target-platform android-arm64`，
        // 产物是 app-arm64-v8a-release.apk；护栏断言该包里只有 arm64-v8a 且含 libyunong_core.so。
        // Uses the version code from pubspec.yaml. When using split APKs, 1000 * ABI_VERSION
        // is added automatically by Flutter. (https://developer.android.com/studio/build/configure-apk-splits#configure-APK-versions)
        // You can force using the value of versionCode by specifying the `-P force-version-code-ignoring-abi=true`
        // flag during build.
        versionCode = flutter.versionCode
        versionName = flutter.versionName
    }

    signingConfigs {
        if (hasReleaseKeystore) {
            create("release") {
                storeFile = file(keystoreProperties.getProperty("storeFile"))
                storePassword = keystoreProperties.getProperty("storePassword")
                keyAlias = keystoreProperties.getProperty("keyAlias")
                keyPassword = keystoreProperties.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            // 有 key.properties 就真签名；没有则退回 debug（本地可用，CI 会用 Secrets 覆盖）
            signingConfig = if (hasReleaseKeystore) {
                signingConfigs.getByName("release")
            } else {
                signingConfigs.getByName("debug")
            }
            // Flutter 默认给 release 打开 shrinkResources；它与 minify 必须同时开/关，
            // 否则 Gradle 直接报 "Removing unused resources requires unused code shrinking"。
            // 这里两个都关（我们的体积主要来自 Flutter 引擎与 .so，资源收缩收益有限）。
            isMinifyEnabled = false
            isShrinkResources = false
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}
