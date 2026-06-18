// Root build file. The Android Gradle Plugin + Kotlin plugin versions are
// declared here (apply false) so the `:qrcodeaiscanner` library module applies
// them without re-declaring versions. Versions per the uniffi mobile plan §2
// (Kotlin 2.0, AGP 8.x for compileSdk 34).
plugins {
    id("com.android.library") version "8.5.2" apply false
    id("org.jetbrains.kotlin.android") version "2.0.20" apply false
}
