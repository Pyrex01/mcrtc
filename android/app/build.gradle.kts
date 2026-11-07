import java.util.Properties

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

/** One NDK for cargo-ndk, the fallback lookup below and the Android Gradle plugin's strip step. */
val rustNdkVersion = "28.2.13676358"

android {
    namespace = "dev.mcrtc"
    compileSdk = 36
    ndkVersion = rustNdkVersion

    defaultConfig {
        applicationId = "dev.mcrtc"
        minSdk = 24
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"

        ndk {
            // Real phones only: the emulator cannot run Minecraft anyway.
            abiFilters += listOf("arm64-v8a")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            // Debug-signed until there is a release key.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
    }

    sourceSets {
        getByName("main") {
            jniLibs.directories.add("build/rustJniLibs")
            kotlin.directories.add("build/generated/uniffi")
        }
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.datastore.preferences)
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.material3)
    implementation(libs.compose.material.icons.core)
    implementation(libs.compose.ui.tooling.preview)
    debugImplementation(libs.compose.ui.tooling)
    // UniFFI's Kotlin bindings talk to the Rust library through JNA.
    implementation("${libs.jna.get()}@aar")

    testImplementation(libs.junit)
}

// --- The Rust core -------------------------------------------------------------------------------

/** The mcrtc repository, which holds the Cargo workspace. */
val repoRoot: File = rootProject.projectDir.parentFile
val rustJniLibs = layout.buildDirectory.dir("rustJniLibs")
val uniffiOut = layout.buildDirectory.dir("generated/uniffi")
val skipRust = providers.gradleProperty("skipRust").map { it.toBoolean() }.getOrElse(false)

val localProperties = Properties().apply {
    val file = rootProject.file("local.properties")
    if (file.exists()) file.inputStream().use { load(it) }
}

val ndkHome: String = localProperties.getProperty("ndk.dir")
    ?: System.getenv("ANDROID_NDK_HOME")
    ?: listOfNotNull(localProperties.getProperty("sdk.dir"), System.getenv("ANDROID_HOME"))
        .map { File(it, "ndk/$rustNdkVersion") }
        .firstOrNull { it.isDirectory }?.path
    ?: ""

/** The Rust sources every Cargo task depends on. */
val rustSources = files(
    File(repoRoot, "Cargo.toml"),
    File(repoRoot, "Cargo.lock"),
    File(repoRoot, "src"),
    File(repoRoot, "android/rust/Cargo.toml"),
    File(repoRoot, "android/rust/uniffi.toml"),
    File(repoRoot, "android/rust/src"),
)

val androidLibrary = rustJniLibs.map { it.file("arm64-v8a/libmcrtc_android.so") }
val bindingsFile = uniffiOut.map { it.file("dev/mcrtc/core/mcrtc_android.kt") }
val bindgenSources = files(
    File(repoRoot, "android/bindgen/Cargo.toml"),
    File(repoRoot, "android/bindgen/src"),
)

val cargoBuild = tasks.register<Exec>("cargoBuild") {
    description = "Cross-compiles the Rust core for every ABI the app ships."
    group = "rust"
    inputs.files(rustSources)
    outputs.dir(rustJniLibs)
    workingDir = repoRoot
    environment("ANDROID_NDK_HOME", ndkHome)
    commandLine(
        "cargo", "ndk",
        "-t", "arm64-v8a",
        "-P", "24",
        "-o", rustJniLibs.get().asFile.path,
        // Always optimized: unoptimized crypto is far too slow to hold a connection. The android
        // profile is release plus the symbol table generateBindings reads; the Android Gradle
        // plugin strips the copy that goes into the APK.
        "build", "--profile", "android", "-p", "mcrtc-android", "--lib",
    )
    // Local copies: a closure that reaches for script values cannot go in the configuration cache.
    val skip = skipRust
    val library = androidLibrary
    val ndk = ndkHome
    onlyIf("skipRust is not set, or there is nothing to reuse") {
        !skip || !library.get().asFile.exists()
    }
    doFirst {
        require(ndk.isNotEmpty()) {
            "No Android NDK found: set ndk.dir in local.properties or ANDROID_NDK_HOME"
        }
    }
}

val generateBindings = tasks.register<Exec>("generateBindings") {
    description = "Generates the Kotlin bindings for the Rust core with UniFFI."
    group = "rust"
    // The bindings come from the phone library itself, so nothing is built for this computer.
    inputs.files(cargoBuild)
    inputs.files(bindgenSources)
    inputs.file(File(repoRoot, "android/rust/uniffi.toml"))
    outputs.dir(uniffiOut)
    workingDir = repoRoot
    commandLine(
        "cargo", "run", "-p", "mcrtc-bindgen", "--",
        "generate", "--library", androidLibrary.get().asFile.path,
        "--language", "kotlin", "--out-dir", uniffiOut.get().asFile.path,
        "--no-format", "--metadata-no-deps",
    )
    val skip = skipRust
    val bindings = bindingsFile
    onlyIf("skipRust is not set, or there is nothing to reuse") {
        !skip || !bindings.get().asFile.exists()
    }
}

tasks.named("preBuild") {
    dependsOn(cargoBuild, generateBindings)
}
