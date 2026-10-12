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
        versionName = "0.2.0"
        versionCode = versionCodeOf(versionName!!)
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
    // X_DISPLAY is the game environment's X display, apart for the two apps because Xvfb's
    // abstract socket is device-wide.
    buildTypes {
        getByName("debug") {
            applicationIdSuffix = ".dev"
            buildConfigField("String", "X_DISPLAY", "\":1\"")
        }
        getByName("release") {
            buildConfigField("String", "X_DISPLAY", "\":0\"")
            signingConfig = signingConfigs.findByName("release")
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
