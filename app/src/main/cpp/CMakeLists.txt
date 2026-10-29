cmake_minimum_required(VERSION 3.22.1)
project(seccompprobe C CXX)

# Every library's LOAD segments are aligned for 4 KB memory pages, the page size that the app's
# Wine and Box64 need. NDK r27 does this by default, but r28 and later align for 16 KB, so the
# option is explicit.
add_link_options("LINKER:-z,max-page-size=4096")

# minSdk is 30 (Android 11): an NDK function from a later API level is a weak reference, and each
# call of one must check the API level first (__builtin_available), or the build stops.
add_compile_options(-D__ANDROID_UNAVAILABLE_SYMBOLS_ARE_WEAK__ -Werror=unguarded-availability)

# Debug probe for the app's seccomp policy: which syscalls it lets through.
add_library(seccompprobe SHARED seccomp_probe.c)
target_compile_options(seccompprobe PRIVATE -Wall -Wextra -Werror)

# The process spawner.
add_library(spawner SHARED spawner.c)
target_compile_options(spawner PRIVATE -Wall -Wextra -Werror)

# The device check's KGSL chip id probe.
add_library(kgslprobe SHARED kgsl_probe.c)
target_compile_options(kgslprobe PRIVATE -Wall -Wextra -Werror)

# The device check's Mali GPU id probe (Arm's kbase).
add_library(maliprobe SHARED mali_probe.c)
target_compile_options(maliprobe PRIVATE -Wall -Wextra -Werror)

# The Game view: the copy of the X screen (frame_copy.c, frame_layer.cpp), the game's direct present (present_server.cpp),
# and the app's clock, the virtual display's vertical blank (frame_clock.c); the rules of the clock and of the direct
# present's newest frame (frame_pacing.c). Host programs check frame_pacing.c and the copy's layer (app/build.gradle.kts).
add_library(framecopy SHARED frame_copy.c frame_layer.cpp present_server.cpp frame_pacing.c frame_clock.c)
target_compile_options(framecopy PRIVATE -Wall -Wextra -Werror -O3)
target_link_libraries(framecopy android nativewindow log)

# The gamepad file's seqlock fence (input/Gamepad.kt).
add_library(inputfence SHARED input_fence.c)
target_compile_options(inputfence PRIVATE -Wall -Wextra -Werror)

# The game's sound (GameAudio.kt): the ring that the game's Wine writes through ALSA (the runtime's
# pd2-audio/pcm_pd2.c, which shares pd2_audio_ring.h), resampled to the phone's rate and played on a
# low-latency AAudio stream. A host program checks the ring and the resampler (app/build.gradle.kts).
add_library(gameaudio SHARED game_audio.c game_audio_resampler.c)
target_include_directories(gameaudio PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/../../../../runtime/pd2-audio)
target_compile_options(gameaudio PRIVATE -Wall -Wextra -Werror -O3)
target_link_libraries(gameaudio aaudio log m)
