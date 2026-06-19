#
# qrcode_ai_scanner — Flutter/iOS plugin podspec.
# The native code is the Rust crate in ../rust, compiled into a static lib by
# cargokit's build_pod.sh (the `script_phase` below) and force-loaded into the
# app. The Classes/ forwarder + ../src C stub only exist to make CocoaPods emit
# a framework (the FFI plugin shim); the real symbols come from the Rust .a.
#
Pod::Spec.new do |s|
  s.name             = 'qrcode_ai_scanner'
  s.version          = '0.3.0'
  s.summary          = 'QR decoding + scannability scoring for artistic / AI-generated QR codes.'
  s.description      = <<-DESC
Native Rust core (via flutter_rust_bridge) for QR decoding + scannability scoring.
                       DESC
  s.homepage         = 'https://github.com/supernovae-st/qrcode-ai-scanner'
  s.license          = { :file => '../LICENSE' }
  s.author           = { 'SuperNovae' => 'studio.supernovae@gmail.com' }
  s.module_name      = 'qrcode_ai_scanner'

  # Classes/ holds a forwarder C file that relatively imports ../src/* so the C
  # framework shim is shared across platforms (podspec can't use relative paths).
  s.source           = { :path => '.' }
  s.source_files = 'Classes/**/*'
  s.dependency 'Flutter'
  s.platform = :ios, '13.0'
  s.swift_version = '5.0'

  # Build the Rust static lib before compiling, then force-load it so the FFI
  # symbols survive dead-stripping. Args: <relative rust dir> <lib name>.
  s.script_phase = {
    :name => 'Build Rust library',
    :script => 'sh "$PODS_TARGET_SRCROOT/../cargokit/build_pod.sh" ../rust qrcode_ai_scanner',
    :execution_position => :before_compile,
    :input_files => ['${BUILT_PRODUCTS_DIR}/cargokit_phony'],
    :output_files => ['${PODS_CONFIGURATION_BUILD_DIR}/qrcode_ai_scanner/libqrcode_ai_scanner.a'],
  }
  s.pod_target_xcconfig = {
    'DEFINES_MODULE' => 'YES',
    # Flutter.framework does not contain an i386 slice.
    'EXCLUDED_ARCHS[sdk=iphonesimulator*]' => 'i386',
    'OTHER_LDFLAGS' => '-force_load ${PODS_CONFIGURATION_BUILD_DIR}/qrcode_ai_scanner/libqrcode_ai_scanner.a',
  }
end
