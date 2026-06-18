# Consumer ProGuard/R8 rules shipped inside the AAR. UniFFI's generated Kotlin
# calls the native lib through JNA via reflection/JNI, so the JNA classes and
# the generated binding interface must survive minification in the consuming
# app. Without these, an R8-minified release app throws at runtime.

# JNA itself (reflection + native callbacks).
-keep class com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.** { public *; }

# The generated UniFFI binding (the *FFI/*Lib JNA library interfaces + records).
# UniFFI 0.31 emits into package `uniffi.<lib-name>` — here
# `uniffi.qrcode_ai_scanner_uniffi` (verified from a host bindgen run).
-keep class uniffi.qrcode_ai_scanner_uniffi.** { *; }
