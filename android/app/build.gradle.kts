plugins { id("com.android.application"); id("org.jetbrains.kotlin.android") }

android { namespace = "com.mcrtc.android"; compileSdk = 35
    defaultConfig {
        applicationId = "com.mcrtc.android"; minSdk = 26; targetSdk = 35; versionCode = 1; versionName = "0.1.0"
        // A universal/fat APK: Gradle keeps each native library under its ABI directory.
        ndk { abiFilters += setOf("arm64-v8a", "armeabi-v7a", "x86_64", "x86") }
    }
    sourceSets["main"].jniLibs.srcDir("src/main/jniLibs")
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

val androidAbis = listOf("arm64-v8a", "armeabi-v7a", "x86_64", "x86")

// Build and package all Rust ABIs with one universal APK when cargo-ndk is installed:
//   MCRTC_BUILD_NATIVE=1 ./gradlew assembleFatDebug
// Keeping this opt-in lets resource-only Android checks run on machines without the NDK.
val buildRustAndroid = tasks.register<Exec>("buildRustAndroid") {
    workingDir(rootProject.projectDir.parentFile)
    commandLine(
        listOf("cargo", "ndk") + androidAbis.flatMap { listOf("-t", it) } +
            listOf("-o", "android/app/src/main/jniLibs", "build", "--release", "-p", "mcrtc-android")
    )
}
if (providers.environmentVariable("MCRTC_BUILD_NATIVE").isPresent) {
    tasks.named("preBuild") { dependsOn(buildRustAndroid) }
}

tasks.register("assembleFatDebug") { dependsOn("assembleDebug") }
tasks.register("assembleFatRelease") { dependsOn("assembleRelease") }

kotlin { jvmToolchain(17) }
