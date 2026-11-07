# UniFFI's bindings reach the Rust library through JNA, which finds methods, fields and callback
# interfaces by reflection, so neither JNA nor the bindings may be renamed or stripped.
-keep class com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.** { public *; }
-keep class dev.mcrtc.core.** { *; }
-dontwarn java.awt.**
-dontwarn java.lang.invoke.**
