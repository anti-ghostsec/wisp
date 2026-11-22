plugins {
    id("com.android.application")
}

// One CPU type per build: arm64-v8a for phones (the default), x86_64 for
// the emulator (-Pabi=x86_64). GeckoView comes in a package per CPU type,
// and the Rust core is built for the same one (build-android.sh).
val abi = (findProperty("abi") as String?) ?: "arm64-v8a"
val geckoview = "160.0.20261116130455"

android {
    namespace = "io.github.wisp.browser"
    // What GeckoView is built against.
    compileSdk {
        version = release(37) { minorApiLevel = 2 }
    }

    defaultConfig {
        applicationId = "io.github.wisp.browser"
        minSdk = 26
        targetSdk = 36
        // Each build must count higher than the one it updates: the release
        // workflow passes the number of commits (-PversionCode=...).
        versionCode = (findProperty("versionCode") as String?)?.toInt() ?: 1
        versionName = (findProperty("versionName") as String?) ?: "0.1.0"
        ndk { abiFilters += abi }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    packaging {
        // Compressed in the APK (Android unpacks them when installing): a
        // download of ~95 MB instead of ~200.
        jniLibs.useLegacyPackaging = true
    }
}

dependencies {
    implementation("org.mozilla.geckoview:geckoview-$abi:$geckoview")
    // Tor, built in: the Guardian Project's build of tor, the one Orbot runs.
    implementation("info.guardianproject:tor-android:0.4.9.17")
    // Bridges: obfs4, Snowflake, meek and webtunnel (Lyrebird and Snowflake, as Orbot uses).
    implementation("com.netzarchitekten:IPtProxy:5.7.0")
    // FileProvider, for sharing downloads on Android 8 and 9 (GeckoView uses it too).
    implementation("androidx.core:core:1.19.1")
    // The Kotlin side of the Rust core's bindings (UniFFI).
    implementation("net.java.dev.jna:jna:5.19.1@aar")
}
