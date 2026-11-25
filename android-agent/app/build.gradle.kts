plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.drapco.agent"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.drapco.agent"
        minSdk = 21
        // targetSdk >= 24: Android 14/15 blokują instalację starszych celów
        targetSdk = 34
        versionCode = 4
        versionName = "1.2"
    }

    signingConfigs {
        create("agent") {
            storeFile = file("../agent.keystore")
            storePassword = "android"
            keyAlias = "agent"
            keyPassword = "android"
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = signingConfigs.getByName("agent")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }
}

// Brak zależności AndroidX - tylko framework Androida + kotlin-stdlib (APK ~50 KB).
