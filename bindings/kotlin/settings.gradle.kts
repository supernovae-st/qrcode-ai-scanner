// Android Gradle project for the qrcode-ai-scanner Kotlin/Android binding.
//
// Layout follows the binding-repo architecture plan (bindings/<lang>/): this is
// the foreign-package-manager shell. The compiled per-ABI `.so` (cargo-ndk) and
// the generated Kotlin (uniffi-bindgen) land INSIDE the `qrcodeaiscanner`
// library module's `src/main/{jniLibs,kotlin}` in CI — they are NOT committed
// (git is not the artifact store; see .gitignore).

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "qrcode-ai-scanner-kotlin"

include(":qrcodeaiscanner")
