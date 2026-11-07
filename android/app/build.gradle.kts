import java.util.Properties
import org.gradle.internal.os.OperatingSystem

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "dev.mcrtc"
    compileSdk = 36

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

    packaging {
        // The .so files are already stripped by the Rust release profile.
        jniLibs.keepDebugSymbols += "**/libmcrtc_android.so"
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
        .map { File(it, "ndk/28.2.13676358") }
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
        // Always optimized: unoptimized crypto is far too slow to hold a connection.
        "build", "--release", "-p", "mcrtc-android", "--lib",
    )
    onlyIf("skipRust is not set, or there is nothing to reuse") {
        !skipRust || !rustJniLibs.get().asFile.resolve("arm64-v8a/libmcrtc_android.so").exists()
    }
    doFirst {
        require(ndkHome.isNotEmpty()) {
            "No Android NDK found: set ndk.dir in local.properties or ANDROID_NDK_HOME"
        }
    }
}

// Release libraries are stripped, so the bindings come from an unstripped host build.
val hostLibrary: String = when {
    OperatingSystem.current().isWindows -> "mcrtc_android.dll"
    OperatingSystem.current().isMacOsX -> "libmcrtc_android.dylib"
    else -> "libmcrtc_android.so"
}
val bindingsFile = uniffiOut.map { it.file("dev/mcrtc/core/mcrtc_android.kt") }

val cargoBuildHost = tasks.register<Exec>("cargoBuildHost") {
    description = "Builds the Rust core for this computer, for generating the bindings."
    group = "rust"
    inputs.files(rustSources)
    outputs.file(File(repoRoot, "target/debug/$hostLibrary"))
    workingDir = repoRoot
    commandLine("cargo", "build", "-p", "mcrtc-android", "--lib")
    onlyIf("skipRust is not set, or there is nothing to reuse") {
        !skipRust || !bindingsFile.get().asFile.exists()
    }
}

val generateBindings = tasks.register<Exec>("generateBindings") {
    description = "Generates the Kotlin bindings for the Rust core with UniFFI."
    group = "rust"
    dependsOn(cargoBuildHost)
    inputs.files(File(repoRoot, "target/debug/$hostLibrary"))
    outputs.dir(uniffiOut)
    workingDir = repoRoot
    commandLine(
        "cargo", "run", "-p", "mcrtc-android", "--bin", "uniffi-bindgen", "--",
        "generate", "--library", "target/debug/$hostLibrary",
        "--language", "kotlin", "--out-dir", uniffiOut.get().asFile.path, "--no-format",
    )
    onlyIf("skipRust is not set, or there is nothing to reuse") {
        !skipRust || !bindingsFile.get().asFile.exists()
    }
}

tasks.named("preBuild") {
    dependsOn(cargoBuild, generateBindings)
}
