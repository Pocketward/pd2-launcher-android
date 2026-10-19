cmake_minimum_required(VERSION 3.22.1)
project(seccompprobe C CXX)

# Every library's LOAD segments are aligned for 16 KB memory pages (NDK r27 links for 4 KB).
add_link_options("LINKER:-z,max-page-size=16384")

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

# The Game view: the copy of the X screen (frame_copy.c, frame_layer.cpp) and the game's direct present (present_server.cpp).
add_library(framecopy SHARED frame_copy.c frame_layer.cpp present_server.cpp)
target_compile_options(framecopy PRIVATE -Wall -Wextra -Werror -O3)
target_link_libraries(framecopy android nativewindow log)

# The gamepad file's seqlock fence (input/Gamepad.kt).
add_library(inputfence SHARED input_fence.c)
target_compile_options(inputfence PRIVATE -Wall -Wextra -Werror)
