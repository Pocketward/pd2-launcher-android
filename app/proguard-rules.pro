# R8 rules of the app (app/build.gradle.kts: the release build, and the debug build with the
# Gradle property pd2.devR8=true). R8 removes the code that the app does not call and keeps what
# these rules and the libraries' own rules name. Each rule below names code that something
# outside the Kotlin code reaches by its name.

# Readable stack traces in the logs that players share: R8 does not rename classes, methods or
# fields, so a stack trace shows real names. R8 still inlines and removes code, can move a method
# to another class and merge classes, writes the line numbers in its own form and names the
# source file r8-map-id-<id>; the build's mapping.txt (app/build/outputs/mapping/) gives the exact
# classes, methods and lines back (retrace).
-dontobfuscate
-keepattributes SourceFile,LineNumberTable

# JNI: the app's native libraries (src/main/cpp: libspawner; libframecopy for FrameCopy,
# PresentServer and FrameClock; libinputfence; libkgslprobe; libmaliprobe; libseccompprobe)
# bind each native method by its Java_<class>_<method> name, so the classes and the native
# methods stay as they are. R8 can still remove what the build does not use (SeccompProbe is for
# debug builds only). The default rules have the same rule for all classes.
-keepclasseswithmembernames,includedescriptorclasses class com.pd2launcher.app.** {
    native <methods>;
}

# JNI: libspawner's JNI_OnLoad finds Spawner (FindClass) and its static onExit(int, boolean, int)
# (GetStaticMethodID) by name, and its reaper threads call onExit. Only native code calls it.
-keep class com.pd2launcher.app.Spawner {
    static void onExit(int, boolean, int);
}

# JNI: zstd-jni (the userland and runtime unpack) has no R8 rules of its own. Its native library
# reads and writes fields of its Java classes by name (srcPos, dstPos, nativePtr, consumed,
# produced) and makes ZstdFrameProgression objects by name (FindClass).
-keep class com.github.luben.zstd.** { *; }

# JSON (kotlinx.serialization): the app gives each serializer by its name in the code (for
# example Release.serializer()), and the generated serializers hold the JSON names as strings.
# The library's own rules (in the kotlinx-serialization-core jar, META-INF/com.android.tools/r8/)
# keep the Companion, serializer() and INSTANCE members that its runtime finds by reflection.

# Enums by name: the default rules (proguard-android-optimize.txt) keep values() and valueOf() of
# every enum. The app finds its stored enum values through entries and name.

# The Android components of AndroidManifest.xml (MainActivity, SessionService, AppUpdateReceiver,
# GameFilesProvider, FileProvider): the build's own rules from the manifest keep them.
