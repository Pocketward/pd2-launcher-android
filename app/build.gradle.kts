import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
}

/**
 * The versionCode of a versionName MAJOR.MINOR.PATCH[-betaN]: ((MAJOR * 100 + MINOR) * 100 +
 * PATCH) * 100 + N, with 99 for the release itself, so every later version has a higher code:
 * 0.1.0-beta1 is 10001, 0.1.0-beta2 10002, 0.1.0 10099, 0.1.1-beta1 10101.
 */
fun versionCodeOf(name: String): Int {
    val match = Regex("""(\d+)\.(\d{1,2})\.(\d{1,2})(?:-beta([1-9]|[1-8]\d|9[0-8]))?""").matchEntire(name)
        ?: error("versionName $name is not MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-betaN")
    val (major, minor, patch, beta) = match.destructured.toList().map { it.toIntOrNull() ?: 99 }
    return ((major * 100 + minor) * 100 + patch) * 100 + beta
}

// Release signing (docs/BUILDING.md): the key stays outside the repository.
// `build.sh release` mounts ~/.config/pd2launcher and names its
// release-signing.properties (storeFile, relative to it; storePassword; keyAlias; keyPassword)
// in PD2_RELEASE_SIGNING.
val releaseSigning: File? = System.getenv("PD2_RELEASE_SIGNING")?.let(::File)?.takeIf { it.isFile }

// The runtime parts the build packs, as their pins name them (runtime/config.sh, userland/config.sh,
// the Box64 patches), for device.txt: "unknown" where a pin is missing.
fun pinned(file: String, name: String): String =
    rootProject.file(file).takeIf { it.isFile }?.readLines()
        ?.firstNotNullOfOrNull { line -> line.trim().takeIf { it.startsWith("$name=") }?.substringAfter('=')?.trim('"') }
        ?: "unknown"
val box64Patches: String = rootProject.file("runtime/patches/box64").list()
    ?.filter { it.endsWith(".patch") }?.map { it.take(4) }?.sorted()?.joinToString(" ")
    ?: "none"
val runtimeParts = "Wine ${pinned("runtime/config.sh", "WINE_VERSION")}, " +
    "Box64 ${pinned("runtime/config.sh", "BOX64_COMMIT").take(8)} with patches $box64Patches, " +
    "Mesa ${pinned("userland/config.sh", "MESA_VERSION")} ${pinned("userland/config.sh", "MESA_COMMIT").take(8)}"

android {
    namespace = "com.pd2launcher.app"
    // compileSdk may be higher than targetSdk; targetSdk is 28 because Android lets an app run
    // programs from its own folder only up to target 28.
    compileSdk = 36
    buildToolsVersion = "36.0.0" // as installed in the build image
    ndkVersion = "27.2.12479018" // r27c, installed in the build image

    defaultConfig {
        applicationId = "com.pd2launcher.app"
        // Android 13, the oldest Android whose app seccomp policy our glibc and FEX answer; above
        // targetSdk, which the plugin warns about but builds.
        minSdk = 33
        targetSdk = 28
        // arm64 only: the App runs the ARM64 engine, so a 32-bit-only phone gets Android's own
        // "not compatible" refusal at install. abiFilters limits our own CMake libraries and the
        // libraries inside dependencies (zstd-jni, androidx.graphics.path) alike.
        ndk { abiFilters += "arm64-v8a" }
        versionName = "0.3.4"
        versionCode = versionCodeOf(versionName!!)
        // App updates (AppUpdater): the public releases the app looks at (the newest 10; it takes
        // the highest version that is not a pre-release), where their files must be, the name its
        // APKs have there (the release workflow's), and the SHA-256 of the certificate they are
        // signed with (the release workflow's key). A debug build looks only at a test feed, whose
        // files may be anywhere, and takes its own certificate.
        buildConfigField("String", "UPDATE_FEED", "\"https://api.github.com/repos/Pocketward/pd2-launcher-android/releases?per_page=10\"")
        buildConfigField("String", "UPDATE_RELEASES", "\"https://github.com/Pocketward/pd2-launcher-android/releases/\"")
        buildConfigField("String", "UPDATE_APK_PREFIX", "\"pd2-launcher-android-\"")
        buildConfigField("String", "UPDATE_CERTIFICATE", "\"90b7d9adf3f0c2c1bac5351f8cc972c9f984704c181f03f8cc206d2711a2972d\"")
        buildConfigField("String", "RUNTIME_PARTS", "\"$runtimeParts\"")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    signingConfigs {
        if (releaseSigning != null) {
            val properties = Properties().apply { releaseSigning.inputStream().use { load(it) } }
            create("release") {
                storeFile = File(releaseSigning.parentFile, properties.getProperty("storeFile"))
                storePassword = properties.getProperty("storePassword")
                keyAlias = properties.getProperty("keyAlias")
                keyPassword = properties.getProperty("keyPassword")
            }
        }
    }
    // The debug build type is the development build: its own application id, so it installs
    // beside the release app, and its own name and launcher mark (src/debug/res). Both carry the
    // same App userland, which each rewrites for its own data folder as it unpacks it.
    // X_DISPLAY is the game environment's X display, apart for the apps because Xvfb's abstract
    // socket is device-wide. This line's development build (0.3) is com.pd2launcher.app.dev3,
    // "PD2 Launcher Dev 0.3", on display :2, so it also installs beside the 0.2 line's development
    // build (com.pd2launcher.app.dev). With the Gradle property pd2.asFexDev=true
    // (build.sh: PD2_DEV_AS_FEX_DEV=1) it builds as com.pd2launcher.app.dev, "PD2 Launcher Dev",
    // display :1, to install over that build: an update in place from it (docs/BUILDING.md).
    val asFexDev = providers.gradleProperty("pd2.asFexDev").map { it.toBoolean() }.getOrElse(false)
    val devName = if (asFexDev) "PD2 Launcher Dev" else "PD2 Launcher Dev 0.3"
    // R8 (code shrinking) for the release build: Android keeps the app's dex code in memory, and
    // R8 removes the code that the app does not call. proguard-rules.pro keeps the names of
    // classes and methods (readable stack traces in shared logs) and the code that native code
    // finds by name. With the Gradle property pd2.devR8=true (build.sh: PD2_DEV_R8=1) the debug
    // build also goes through R8, for device tests of the shrunk code; for a debuggable build R8
    // only removes code and does not optimize it.
    val devR8 = providers.gradleProperty("pd2.devR8").map { it.toBoolean() }.getOrElse(false)
    buildTypes {
        getByName("debug") {
            applicationIdSuffix = if (asFexDev) ".dev" else ".dev3"
            buildConfigField("String", "X_DISPLAY", if (asFexDev) "\":1\"" else "\":2\"")
            resValue("string", "app_name", devName)
            resValue("string", "files_root_title", devName)
            if (devR8) {
                isMinifyEnabled = true
                proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            }
        }
        getByName("release") {
            buildConfigField("String", "X_DISPLAY", "\":0\"")
            signingConfig = signingConfigs.findByName("release")
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }

    externalNativeBuild {
        cmake {
            path = file("src/main/cpp/CMakeLists.txt")
            version = "3.22.1" // as installed in the build image
        }
    }

    // The Launcher engine: the standalone ARM64 Launcher (engine/build.sh),
    // staged by build.sh as engine/arm64-v8a/libpd2launcher.so (gitignored). As a native
    // library it is extracted executable into nativeLibraryDir and replaced with each App update;
    // the session service starts it through the App userland's loader.
    sourceSets["main"].jniLibs.srcDir("engine")

    packaging {
        jniLibs {
            // Extract native libraries at install, so the engine exists as a file to run.
            useLegacyPackaging = true
            // Ship the engine byte-identical to the built artifact (it is already stripped).
            keepDebugSymbols += "**/libpd2launcher.so"
        }
    }

    androidResources {
        // The App userland archive (userland/pack.sh) is already zstd-compressed and is
        // streamed from the APK; storing it uncompressed avoids a second inflate.
        noCompress += "zst"
    }

    lint {
        // targetSdk 28 is deliberate; the lint checks for old targets do not apply.
        disable += "ExpiredTargetSdkVersion"
        disable += "OldTargetApi"
    }
}

// A release build is never left unsigned (the plugin would make app-release-unsigned.apk).
tasks.matching { it.name == "preReleaseBuild" }.configureEach {
    doFirst {
        if (releaseSigning == null) {
            throw GradleException(
                "release signing is missing: PD2_RELEASE_SIGNING must name the release-signing.properties " +
                    "of the release key (build.sh release; docs/BUILDING.md)",
            )
        }
    }
}

// The Game view's native rules, checked by host programs before the JVM tests: the app's clock and
// the direct present's newest frame (src/main/cpp/frame_pacing.c, by
// src/test/cpp/frame_pacing_test.c, with Mesa patch 0008's rule written out), and its copy layer's
// buffers (src/main/cpp/frame_layer.cpp and the copy that uses it, frame_copy.c) and the direct
// present's newest frame (src/main/cpp/present_server.cpp), against stub Android APIs, by
// src/test/cpp/frame_layer_test.cpp; and the game's sound path: the ring between Wine's ALSA plugin
// and the app (../runtime/pd2-audio/pd2_audio_ring.h) and the resampler
// (src/main/cpp/game_audio_resampler.c, by src/test/cpp/game_audio_test.c). The NDK's clang builds
// each as a static x86_64 program, which the build image (linux/amd64) runs. Skipped where a test,
// the NDK's Linux x86_64 clang or a Linux x86_64 host to run the program is not.
val nativeHostTestPrograms = mapOf(
    "frame_pacing_test" to listOf("src/test/cpp/frame_pacing_test.c", "src/main/cpp/frame_pacing.c"),
    "frame_layer_test" to listOf(
        "src/test/cpp/frame_layer_test.cpp", "src/test/cpp/frame_copy_harness.c", "src/main/cpp/frame_layer.cpp",
        "src/main/cpp/frame_pacing.c", "src/test/cpp/present_server_harness.cpp",
    ),
    "game_audio_test" to listOf("src/test/cpp/game_audio_test.c", "src/main/cpp/game_audio_resampler.c"),
)
// Read only when a host-test task is configured: not on every Gradle run.
val nativeHostClang by lazy { android.ndkDirectory.resolve("toolchains/llvm/prebuilt/linux-x86_64/bin/clang") }
val nativeHostIsLinuxX64 = System.getProperty("os.name") == "Linux" && System.getProperty("os.arch") in setOf("amd64", "x86_64")
val nativeHostTests by tasks.registering
for ((name, paths) in nativeHostTestPrograms) {
    val sources = paths.map { file(it) }
    val program = layout.buildDirectory.file("native-host-tests/$name").get().asFile
    val build = tasks.register<Exec>("build_$name") {
        onlyIf { sources[0].isFile && nativeHostClang.canExecute() && nativeHostIsLinuxX64 }
        // With every header and C file they read: frame_copy_harness.c includes frame_copy.c.
        inputs.files(sources + fileTree("src/main/cpp") + fileTree("../runtime/pd2-audio"))
        outputs.file(program)
        doFirst { program.parentFile.mkdirs() }
        commandLine(
            listOf(
                nativeHostClang.path,
                "--target=x86_64-linux-android33", "-static", "-O2", "-Wall", "-Wextra", "-Werror",
                "-fno-exceptions", "-fno-rtti", "-I", file("src/main/cpp").path,
                "-I", file("../runtime/pd2-audio").path, "-o", program.path,
            ) + sources.map { it.path } + "-lm",
        )
    }
    val run = tasks.register<Exec>("run_$name") {
        dependsOn(build)
        onlyIf { sources[0].isFile && nativeHostClang.canExecute() && nativeHostIsLinuxX64 }
        commandLine(program.path)
    }
    nativeHostTests { dependsOn(run) }
}
tasks.matching { it.name == "testDebugUnitTest" }.configureEach { dependsOn(nativeHostTests) }

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2026.06.01"))
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.activity:activity-compose:1.12.4")
    implementation("androidx.core:core-ktx:1.16.0")
    // Control channel: JSON messages and the snapshot StateFlow.
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.11.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
    // App userland unpack: streaming zstd decompression of the archive asset. zstd-jni wraps the
    // reference C zstd (the same library pack.sh compresses with), ships arm64 builds in its AAR
    // and streams without loading the archive into memory. Pinned exactly: 1.5.7-12 is the newest
    // release whose AAR does not require compileSdk 37. The JVM unit tests use plain tar.
    implementation("com.github.luben:zstd-jni:1.5.7-12@aar")

    testImplementation("junit:junit:4.13.2")
}
