# Builds only the userspace RESOLVE_IN_ROOT resolver (FEX patch 0002) and its Catch2 test, from a
# patched FEX tree, without the rest of FEX:
#   cmake -S resolver-test -B <build> -DFEX_SOURCE=<patched FEX tree> && cmake --build <build> && <build>/InRootResolver
cmake_minimum_required(VERSION 3.16)
project(InRootResolverTest CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

if (NOT FEX_SOURCE)
  message(FATAL_ERROR "set FEX_SOURCE to the patched FEX source tree")
endif()

add_subdirectory(${FEX_SOURCE}/External/Catch2 Catch2 EXCLUDE_FROM_ALL)

add_executable(InRootResolver
  ${FEX_SOURCE}/unittests/APITests/InRootResolver.cpp
  ${FEX_SOURCE}/Source/Tools/LinuxEmulation/LinuxSyscalls/InRootResolver.cpp)
target_include_directories(InRootResolver PRIVATE ${FEX_SOURCE}/Source/Tools/LinuxEmulation)
target_compile_options(InRootResolver PRIVATE -Wall -Werror)
target_link_libraries(InRootResolver PRIVATE Catch2::Catch2WithMain)
