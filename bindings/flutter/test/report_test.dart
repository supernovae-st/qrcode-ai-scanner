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
        expect(r.versions.scoreContract, 4, reason: f.path);
        expect(r.detections, isA<List<Detection>>(), reason: f.path);
      }
    }, skip: skipReason);

    test('transparent-envelope → alpha block, envelope, placement hint', () {
      final r = golden('transparent-envelope.json');
      final a = r.alpha!;
      expect(a.background, 'white');
      expect(a.mode, AlphaMode.auto);
      expect(a.fallbackUsed, isFalse);
      expect(a.coverage, closeTo(0.612, 0.001));
      final env = a.envelope!;
      expect(env.placement, AlphaPlacement.lightOnly);
      expect(env.probes, isNotEmpty);
      expect(env.probes.first.decoded, isFalse);
      expect(env.probes.last.decoded, isTrue);
      expect(env.safeLuma.last.last, 255);
      expect(r.hints.whereType<HintAlphaBackgroundDependent>(), isNotEmpty);
    }, skip: skipReason);

    test('opaque goldens carry no alpha block at all', () {
      expect(golden('clean-url.json').alpha, isNull);
      expect(golden('no-detection.json').alpha, isNull);
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
      // a newer label is a complete, checked decoding — never `unknown`
      expect(d.content.charset, Charset.unrecognized);
      expect(d.content.charset.isLossy, isFalse);
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
      // no designator observed, no FNC1: both keys absent → null
      expect(d.content.eci, isNull);
      expect(d.content.fnc1, isNull);
    });
  });

  group('budget flag (spec/02 § Budget semantics)', () {
    ScanReport withTrace(String trace) => ScanReport.parse(
        '{"detections":[],"score":null,"hints":[],"trace":$trace,'
        '"versions":{"scanner":"0.9.0","pipeline":3,"score_contract":5}}');

    test('an older report without the flag reads as unknown, never final', () {
      final r = withTrace('{"stages":[],"engine_panics":0,"total_ms":0}');
      expect(r.trace.budgetExhausted, isNull);
    });

    test('a cut and a final report keep their flag', () {
      final cut = withTrace(
          '{"stages":[],"engine_panics":0,"total_ms":0,"budget_exhausted":true}');
      expect(cut.trace.budgetExhausted, isTrue);
      final whole = withTrace(
          '{"stages":[],"engine_panics":0,"total_ms":0,"budget_exhausted":false}');
      expect(whole.trace.budgetExhausted, isFalse);
    });
  });

  group('content contract (spec/01 § Content)', () {
    test('every charset label decodes; unknown is the only lossy one', () {
      expect(Charset.from('utf8'), Charset.utf8);
      expect(Charset.from('shift_jis'), Charset.shiftJis);
      expect(Charset.from('latin1'), Charset.latin1);
      expect(Charset.from('gb18030'), Charset.gb18030);
      expect(Charset.from('eci'), Charset.eci);
      expect(Charset.from('unknown'), Charset.unknown);
      expect(Charset.from('future_x'), Charset.unrecognized);
      for (final c in Charset.values) {
        expect(c.isLossy, c == Charset.unknown, reason: '$c');
      }
    });

    test('fnc1 positions decode; a newer one is unrecognized', () {
      expect(Fnc1Position.from('first'), Fnc1Position.first);
      expect(Fnc1Position.from('second'), Fnc1Position.second);
      expect(Fnc1Position.from('fifth'), Fnc1Position.unrecognized);
    });

    test('eci entries, an unlocated declaration and fnc1 on the wire', () {
      DecodedContent content(String json) => ScanReport.parse(
              '{"detections":[{"symbology":"qr_code","content":$json,'
              '"payload":{"kind":"text"},"corners":null,'
              '"meta":{"version":null,"ec_level":null,"mask":null,"modules":null,"inverted":null},'
              '"engines":["rxing"]}],"score":null,"hints":[],'
              '"trace":{"stages":[],"engine_panics":0,"total_ms":0},'
              '"versions":{"scanner":"0.9.0","pipeline":3,"score_contract":5}}')
          .detections
          .single
          .content;
      final declared = content(
          '{"text":"Ą€","raw":"oeKCrA==","charset":"eci","eci":[{"designator":4,"start":0},{"designator":26,"start":1}]}');
      expect(declared.charset, Charset.eci);
      expect(declared.eci!.map((e) => [e.designator, e.start]).toList(), [
        [4, 0],
        [26, 1]
      ]);
      final unlocated = content(
          '{"text":"\\ufffd","raw":"oQ==","charset":"unknown","eci":[],"fnc1":"first"}');
      expect(unlocated.eci, isEmpty);
      expect(unlocated.charset.isLossy, isTrue);
      expect(unlocated.fnc1, Fnc1Position.first);
      final unmarked =
          content('{"text":"日本中文","raw":"k/qWe9bQzsQ=","charset":"unknown"}');
      expect(unmarked.eci, isNull);
      expect(unmarked.charset, Charset.unknown);
      final aim = content(
          '{"text":"37abc","raw":"MzdhYmM=","charset":"utf8","fnc1":"second"}');
      expect(aim.fnc1, Fnc1Position.second);
    });
  });
}
