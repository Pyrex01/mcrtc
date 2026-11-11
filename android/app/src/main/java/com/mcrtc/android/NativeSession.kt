package com.mcrtc.android

/** Narrow JNI contract; Tokio, WebRTC objects, and Rust channels never cross this boundary. */
object NativeSession {
    private var loaded = false
    init {
        try {
            System.loadLibrary("mcrtc_android")
            loaded = true
        } catch (_: UnsatisfiedLinkError) {
            // The UI reports this as an actionable setup error; never crash on a missing ABI.
        }
    }
    fun isAvailable(): Boolean = loaded
    external fun create(): Long
    external fun command(handle: Long, command: String, value: String): String
    external fun poll(handle: Long): String?
    external fun destroy(handle: Long)
}
