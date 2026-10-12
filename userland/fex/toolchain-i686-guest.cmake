# i686 guest toolchain for FEX's 32-bit thunk libraries (ThunkLibs/GuestLibs, BITNESS=32).
# Give it to FEX's configure as -DX86_32_TOOLCHAIN_FILE=<this file>. It cross-compiles with
# clang + lld against a Debian x86 dev sysroot that also carries the i386 multiarch packages
# (libc6-dev:i386, libgcc-14-dev:i386, libstdc++-14-dev:i386, libx11-dev:i386, libgl-dev:i386,
# libegl-dev:i386, libglx-dev:i386, ...). X86_SYSROOT names that sysroot.
if (NOT DEFINED ENV{X86_SYSROOT})
  message(FATAL_ERROR "set X86_SYSROOT to the x86 dev sysroot with the i386 packages")
endif()
set(SYSROOT_DIR "$ENV{X86_SYSROOT}")

set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR i686)
set(CMAKE_C_COMPILER clang)
set(CMAKE_CXX_COMPILER clang++)
set(CMAKE_C_COMPILER_TARGET i686-linux-gnu)
set(CMAKE_CXX_COMPILER_TARGET i686-linux-gnu)
set(CMAKE_C_COMPILER_AR llvm-ar)
set(CMAKE_CXX_COMPILER_AR llvm-ar)
set(CMAKE_C_COMPILER_RANLIB llvm-ranlib)
set(CMAKE_CXX_COMPILER_RANLIB llvm-ranlib)
foreach(kind EXE SHARED MODULE)
  set(CMAKE_${kind}_LINKER_FLAGS_INIT "-fuse-ld=lld")
endforeach()

set(CMAKE_SYSROOT "${SYSROOT_DIR}")
set(CMAKE_FIND_ROOT_PATH "${SYSROOT_DIR}")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)

# pkg-config answers for i386 from the sysroot only.
set(ENV{PKG_CONFIG_LIBDIR} "${SYSROOT_DIR}/usr/lib/i386-linux-gnu/pkgconfig:${SYSROOT_DIR}/usr/share/pkgconfig")
set(ENV{PKG_CONFIG_SYSROOT_DIR} "${SYSROOT_DIR}")
set(ENV{PKG_CONFIG_PATH} "")

# SSE math as FEX's own i686 toolchain uses. No stack protector: the 32-bit guest thunks get
# the host function address in mm0 and the canary code must not disturb the thunk ABI.
set(CMAKE_C_FLAGS_INIT "-msse2 -mfpmath=sse -fno-stack-protector")
set(CMAKE_CXX_FLAGS_INIT "-msse2 -mfpmath=sse -fno-stack-protector")
