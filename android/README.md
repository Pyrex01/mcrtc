# mcrtc for Android

Join a Minecraft world that a friend hosts with mcrtc on a PC. Paste their invite, send
back the reply, then play with Minecraft pointed at `127.0.0.1:19132`.

## Prerequisites

- Android SDK with platform 36 and NDK 28.2.13676358. Put the SDK path in
  `android/local.properties` (`sdk.dir=...`). The NDK is found at `<sdk>/ndk/28.2.13676358`, or
  set `ndk.dir` there, or `ANDROID_NDK_HOME`.
- JDK 17 or newer.
- Rust with the Android targets and cargo-ndk:

  ```
  rustup target add aarch64-linux-android
  cargo install cargo-ndk
  ```

## Build

From `android/`:

```
./gradlew assembleDebug
```

This cross-compiles the Rust core (`android/rust`) for arm64-v8a (64-bit ARM phones), always optimized,
generates the Kotlin bindings with UniFFI, and builds the app. The APK lands in
`app/build/outputs/apk/debug/app-debug.apk`.

`./gradlew assembleRelease` builds a minified APK in `app/build/outputs/apk/release/`, signed
with the debug key for now.

When only the Kotlin changes, `-PskipRust=true` skips the Rust tasks as long as their outputs
already exist.

Unit tests: `./gradlew testDebugUnitTest`.

## Install

With USB debugging on:

```
adb install -r app/build/outputs/apk/debug/app-debug.apk
```
