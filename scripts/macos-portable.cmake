# Release packages must run on baseline CPUs, not only the CI host.
set(GGML_NATIVE OFF CACHE BOOL "Portable CPU backend" FORCE)
if(CMAKE_SYSTEM_PROCESSOR MATCHES "^(arm64|aarch64|ARM64)$" OR CMAKE_OSX_ARCHITECTURES STREQUAL "arm64")
  set(GGML_CPU_ARM_ARCH "armv8-a" CACHE STRING "Apple Silicon baseline" FORCE)
endif()
