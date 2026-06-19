// On-device integration test: exercises the REAL native core (the parts the
// host-only test/report_test.dart can't). Run on an emulator/device:
//   cd example && flutter test integration_test
// CI builds the example (cargokit compiles the Rust); running this needs a
// device, so it is not part of the build-only CI jobs.

import 'dart:typed_data';

import 'package:flutter/services.dart' show rootBundle;
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:qrcode_ai_scanner/qrcode_ai_scanner.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  setUpAll(() async => QrcodeAiScanner.init());

  testWidgets('decodes the bundled QR through the native core', (tester) async {
    final bytes =
        (await rootBundle.load('assets/sample_qr.png')).buffer.asUint8List();
    final report = await QrcodeAiScanner.scan(bytes, profile: 'full');
    expect(report.detections, isNotEmpty);
    expect(report.detections.first.content.text, isNotEmpty);
    expect(report.detections.first.symbology, Symbology.qrCode);
    expect(report.versions.scoreContract, 3);
  });

  testWidgets('a blank RGBA frame is "no QR", not an error', (tester) async {
    final blank = Uint8List(16 * 16 * 4)..fillRange(0, 16 * 16 * 4, 0xFF);
    final report = await QrcodeAiScanner.scanFrame(blank, 16, 16);
    expect(report.detections, isEmpty);
    expect(report.score, isNull); // frame profile skips scoring
  });

  testWidgets('invalid bytes throw with the QRS-001 wire code', (tester) async {
    await expectLater(
      QrcodeAiScanner.scan(Uint8List.fromList([0xDE, 0xAD, 0xBE, 0xEF])),
      throwsA(predicate((e) => '$e'.contains('QRS-001'))),
    );
  });
}
