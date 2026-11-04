# Release packages must run on baseline CPUs, not only the CI host.
set(GGML_NATIVE OFF CACHE BOOL "Portable CPU backend" FORCE)
# Backport upstream #457 (7656d7b) without its incompatible whisper-cpp-rs migration.
# Use ggml's pthread pool instead of depending on the build machine's OpenMP runtime.
set(GGML_OPENMP OFF CACHE BOOL "No external OpenMP runtime in macOS app" FORCE)
if(CMAKE_SYSTEM_PROCESSOR MATCHES "^(arm64|aarch64|ARM64)$" OR CMAKE_OSX_ARCHITECTURES STREQUAL "arm64")
  set(GGML_CPU_ARM_ARCH "armv8-a" CACHE STRING "Apple Silicon baseline" FORCE)
endif()
