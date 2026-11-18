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
        versionCode = 1
        versionName = "0.1.0"
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
        jniLibs.useLegacyPackaging = false
    }
}

dependencies {
    implementation("org.mozilla.geckoview:geckoview-$abi:$geckoview")
    // The Kotlin side of the Rust core's bindings (UniFFI).
    implementation("net.java.dev.jna:jna:5.19.1@aar")
}
