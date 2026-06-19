// Pins lib/report.dart against the canonical wire contract. Runs on the host
// (no native lib) — it exercises ScanReport.fromJson on the repo's golden
// examples (spec/examples/, CI-validated against scan-report.schema.json) plus
// hand-crafted unknown-value inputs that MUST decode without throwing.
//
// The native scan path (QrcodeAiScanner.scan) is covered by the example app's
// integration_test on a real device/emulator (CI), not here.

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:qrcode_ai_scanner/report.dart';

void main() {
  final examples = Directory('../../spec/examples');
  final hasGoldens = examples.existsSync();
  final skipReason =
      hasGoldens ? null : 'spec/examples goldens absent (run from the repo)';

  ScanReport golden(String name) =>
      ScanReport.parse(File('${examples.path}/$name').readAsStringSync());

  group('canonical spec goldens', () {
    test('every golden decodes without throwing + keeps the envelope', () {
      final files = examples
          .listSync()
          .whereType<File>()
          .where((f) => f.path.endsWith('.json'))
          .toList();
      expect(files, isNotEmpty);
      for (final f in files) {
        final r = ScanReport.parse(f.readAsStringSync());
        expect(r.versions.scoreContract, 3, reason: f.path);
        expect(r.detections, isA<List<Detection>>(), reason: f.path);
      }
    }, skip: skipReason);

    test('clean-url → typed url payload + grades', () {
      final d = golden('clean-url.json').detections.single;
      expect(d.symbology, Symbology.qrCode);
      expect(d.content.text, 'https://qrc-ai.com/76xMa');
      expect(d.payload, isA<PayloadUrl>());
      expect((d.payload as PayloadUrl).url, 'https://qrc-ai.com/76xMa');
    }, skip: skipReason);

    test('clean-url → score grades decode to enums', () {
      final s = golden('clean-url.json').score!;
      expect(s.grade, Grade.excellent);
      expect(s.uec!.grade, LetterGrade.a);
    }, skip: skipReason);

    test('crypto-bitcoin → PayloadCrypto fields', () {
      final p = golden('crypto-bitcoin.json').detections.single.payload;
      expect(p, isA<PayloadCrypto>());
      p as PayloadCrypto;
      expect(p.scheme, 'bitcoin');
      expect(p.address, startsWith('bc1q'));
      expect(p.amount, '0.01');
    }, skip: skipReason);

    test('no-detection → empty detections, null score', () {
      final r = golden('no-detection.json');
      expect(r.detections, isEmpty);
      expect(r.score, isNull);
    }, skip: skipReason);
  });

  group('additive tolerance — unknown values never throw (#[non_exhaustive])', () {
    test('unknown enum / payload kind / hint fall back', () {
      const json = '''
      {
        "detections": [{
          "symbology": "future_symbology",
          "content": {"text": "x", "raw": "", "charset": "future_charset"},
          "payload": {"kind": "future_kind", "weird": 1},
          "corners": null,
          "meta": {"version": null, "ec_level": null, "mask": null, "modules": null, "inverted": null},
          "engines": ["future_engine"]
        }],
        "score": null,
        "hints": [{"hint": "future_hint", "x": 2}],
        "trace": {"stages": [], "engine_panics": 0, "total_ms": 0},
        "versions": {"scanner": "9.9.9", "pipeline": 99, "score_contract": 99}
      }''';
      final r = ScanReport.parse(json);
      final d = r.detections.single;
      expect(d.symbology, Symbology.unknown);
      expect(d.content.charset, Charset.unknown);
      expect(d.payload, isA<PayloadUnknown>());
      expect((d.payload as PayloadUnknown).kind, 'future_kind');
      expect(d.engines.single, EngineKind.unknown);
      expect(d.meta.inverted, isNull);
      expect(r.hints.single, isA<HintUnknown>());
    });

    test('base64 raw decodes to bytes; bad base64 → empty (no throw)', () {
      const ok =
          '{"detections":[{"symbology":"qr_code","content":{"text":"hi","raw":"aGk=","charset":"utf8"},"payload":{"kind":"text"},"corners":null,"meta":{"version":1,"ec_level":"m","mask":0,"modules":21,"inverted":false},"engines":["rxing"]}],"score":null,"hints":[],"trace":{"stages":[],"engine_panics":0,"total_ms":0},"versions":{"scanner":"0.3.0","pipeline":1,"score_contract":3}}';
      final d = ScanReport.parse(ok).detections.single;
      expect(d.content.raw, [0x68, 0x69]); // "hi"
      expect(d.payload, isA<PayloadText>());
      expect(d.meta.ecLevel, EcLevel.m);
    });
  });
}
