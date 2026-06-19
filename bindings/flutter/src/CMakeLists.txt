# The Flutter tooling requires that developers have CMake 3.10 or later
# installed. You should not increase this version, as doing so will cause
# the plugin to fail to compile for some customers of the plugin.
cmake_minimum_required(VERSION 3.10)

project(qrcode_ai_scanner_library VERSION 0.0.1 LANGUAGES C)

add_library(qrcode_ai_scanner SHARED
  "qrcode_ai_scanner.c"
)

set_target_properties(qrcode_ai_scanner PROPERTIES
  PUBLIC_HEADER qrcode_ai_scanner.h
  OUTPUT_NAME "qrcode_ai_scanner"
)

target_compile_definitions(qrcode_ai_scanner PUBLIC DART_SHARED_LIB)

if (ANDROID)
  # Support Android 15 16k page size
  target_link_options(qrcode_ai_scanner PRIVATE "-Wl,-z,max-page-size=16384")
endif()
