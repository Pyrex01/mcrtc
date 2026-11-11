plugins { id("com.android.application"); id("org.jetbrains.kotlin.android") }

android {
    namespace = "com.mcrtc.android"
    compileSdk = 35
    defaultConfig {
        applicationId = "com.mcrtc.android"
        minSdk = 26
        targetSdk = 35
        versionCode = 2
        versionName = "0.1.0"
        // One universal APK: libwebrtc ships a native library for each of these.
        ndk { abiFilters += setOf("arm64-v8a", "armeabi-v7a", "x86_64", "x86") }
    }
    // The desktop app's Monocraft font, so both frontends draw the same letters.
    sourceSets["main"].assets.srcDir("../../assets/ui")
    buildFeatures { buildConfig = true }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation("io.github.webrtc-sdk:android:144.7559.15")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
    testImplementation("junit:junit:4.13.2")
    // Android's own org.json is only a stub in JVM unit tests.
    testImplementation("org.json:json:20240303")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.9.0")
}

kotlin { jvmToolchain(17) }
