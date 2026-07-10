// Typed Dart mirror of the ScanReport wire contract.
//
// SSOT: `spec/scan-report.schema.json` + `bindings/report-types.d.ts`. This file
// ports that contract field-for-field. The native side returns a JSON string
// (the one serde contract every binding shares — py/node/wasm/uniffi); the
// facade `jsonDecode`s it and hands the map to `ScanReport.fromJson`.
//
// Additive tolerance is MANDATORY (the Rust enums are `#[non_exhaustive]`, the
// schema says consumers MUST accept unknown values): every enum has an
// `unknown` fallback and `Payload`/`Hint` have catch-all variants that keep the
// raw map. `fromJson` NEVER throws on an unknown enum/kind.
//
// R2: `scripts/sync-report-types` could emit this from the same SSOT; for now
// it is hand-maintained, pinned by the round-trip test in test/report_test.dart.

import 'dart:convert';
import 'dart:typed_data';

// ─────────────────────────── enums ───────────────────────────

enum Symbology {
  qrCode,
  microQrCode,
  rectangularMicroQrCode,
  dataMatrix,
  aztec,
  pdf417,
  maxiCode,
  ean13,
  ean8,
  upcA,
  upcE,
  code128,
  code39,
  code93,
  codabar,
  itf,
  dataBar,
  dataBarExpanded,
  telepen,
  unknown;

  static Symbology from(Object? v) => switch (v) {
        'qr_code' => qrCode,
        'micro_qr_code' => microQrCode,
        'rectangular_micro_qr_code' => rectangularMicroQrCode,
        'data_matrix' => dataMatrix,
        'aztec' => aztec,
        'pdf417' => pdf417,
        'maxi_code' => maxiCode,
        'ean13' => ean13,
        'ean8' => ean8,
        'upc_a' => upcA,
        'upc_e' => upcE,
        'code128' => code128,
        'code39' => code39,
        'code93' => code93,
        'codabar' => codabar,
        'itf' => itf,
        'data_bar' => dataBar,
        'data_bar_expanded' => dataBarExpanded,
        'telepen' => telepen,
        _ => unknown,
      };
}

enum EngineKind {
  rxing,
  rqrr,
  rescue,
  unknown;

  static EngineKind from(Object? v) => switch (v) {
        'rxing' => rxing,
        'rqrr' => rqrr,
        'rescue' => rescue,
        _ => unknown,
      };
}

enum EcLevel {
  l,
  m,
  q,
  h,
  unknown;

  static EcLevel from(Object? v) => switch (v) {
        'l' => l,
        'm' => m,
        'q' => q,
        'h' => h,
        _ => unknown,
      };
}

enum Charset {
  utf8,
  shiftJis,
  latin1,
  unknown;

  static Charset from(Object? v) => switch (v) {
        'utf8' => utf8,
        'shift_jis' => shiftJis,
        'latin1' => latin1,
        _ => unknown,
      };
}

/// The 0–100 scannability band.
enum Grade {
  excellent,
  good,
  acceptable,
  fair,
  poor,
  unknown;

  static Grade from(Object? v) => switch (v) {
        'excellent' => excellent,
        'good' => good,
        'acceptable' => acceptable,
        'fair' => fair,
        'poor' => poor,
        _ => unknown,
      };
}

/// ISO 15415 / UEC letter grade (A…F). Shared by `UecReport.grade`,
/// `Iso15415Report.overall`, and each `IsoParameter.grade`.
enum LetterGrade {
  a,
  b,
  c,
  d,
  f,
  unknown;

  static LetterGrade from(Object? v) => switch (v) {
        'a' => a,
        'b' => b,
        'c' => c,
        'd' => d,
        'f' => f,
        _ => unknown,
      };
}

enum StressAxis {
  resolution,
  blur,
  contrast,
  perspective,
  rotation,
  lighting,
  unknown;

  static StressAxis from(Object? v) => switch (v) {
        'resolution' => resolution,
        'blur' => blur,
        'contrast' => contrast,
        'perspective' => perspective,
        'rotation' => rotation,
        'lighting' => lighting,
        _ => unknown,
      };
}

// ─────────────────────────── helpers ───────────────────────────

Map<String, dynamic> _obj(Object? v) =>
    v is Map ? v.cast<String, dynamic>() : const {};
List<Map<String, dynamic>> _objList(Object? v) =>
    v is List ? v.map(_obj).toList() : const [];
List<String> _strList(Object? v) =>
    v is List ? v.map((e) => '$e').toList() : const [];
int? _int(Object? v) => v is num ? v.toInt() : null;
double _double(Object? v) => v is num ? v.toDouble() : 0;
String _str(Object? v) => v is String ? v : '';

// ─────────────────────────── leaf types ───────────────────────────

class Point {
  final double x, y;
  const Point(this.x, this.y);
  factory Point.fromJson(Map<String, dynamic> j) =>
      Point(_double(j['x']), _double(j['y']));
}

class Gs1Element {
  final String ai, value;
  const Gs1Element(this.ai, this.value);
  factory Gs1Element.fromJson(Map<String, dynamic> j) =>
      Gs1Element(_str(j['ai']), _str(j['value']));
}

class DecodedContent {
  final String text;

  /// Original payload bytes (base64-decoded from the wire `raw` string).
  final Uint8List raw;
  final Charset charset;
  const DecodedContent(this.text, this.raw, this.charset);

  factory DecodedContent.fromJson(Map<String, dynamic> j) {
    Uint8List raw;
    try {
      raw = base64.decode(_str(j['raw']));
    } catch (_) {
      raw = Uint8List(0);
    }
    return DecodedContent(_str(j['text']), raw, Charset.from(j['charset']));
  }
}

class QrMeta {
  final int? version;
  final EcLevel? ecLevel;
  final int? mask;
  final int? modules;

  /// Symbol is photometrically inverted (light-on-dark); null when ungeometried.
  final bool? inverted;
  const QrMeta(
      this.version, this.ecLevel, this.mask, this.modules, this.inverted);

  factory QrMeta.fromJson(Map<String, dynamic> j) => QrMeta(
        _int(j['version']),
        j['ec_level'] == null ? null : EcLevel.from(j['ec_level']),
        _int(j['mask']),
        _int(j['modules']),
        j['inverted'] as bool?,
      );
}

// ─────────────────────────── Payload (tagged union) ───────────────────────────

/// Typed payload classification. Unknown `kind`s decode to [PayloadUnknown]
/// (never a throw) — the schema mandates additive tolerance.
sealed class Payload {
  /// The wire discriminator (`url` · `wifi` · … · or an unrecognised value).
  String get kind;

  factory Payload.fromJson(Map<String, dynamic> j) {
    final kind = _str(j['kind']);
    return switch (kind) {
      'url' => PayloadUrl(_str(j['url'])),
      'wifi' => PayloadWifi(
          ssid: _str(j['ssid']),
          security: _str(j['security']),
          password: j['password'] as String?,
          hidden: j['hidden'] == true,
        ),
      'email' => PayloadEmail(
          to: _str(j['to']),
          subject: j['subject'] as String?,
          body: j['body'] as String?,
        ),
      'sms' => PayloadSms(number: _str(j['number']), body: j['body'] as String?),
      'tel' => PayloadTel(_str(j['number'])),
      'geo' => PayloadGeo(_double(j['lat']), _double(j['lon'])),
      'gs1' => PayloadGs1(
          elements: _objList(j['elements']).map(Gs1Element.fromJson).toList(),
          gtin: j['gtin'] as String?,
          conformant: j['conformant'] == true,
          issues: _strList(j['issues']),
        ),
      'gs1_digital_link' => PayloadGs1DigitalLink(
          url: _str(j['url']),
          elements: _objList(j['elements']).map(Gs1Element.fromJson).toList(),
          gtin: j['gtin'] as String?,
          conformant: j['conformant'] == true,
          issues: _strList(j['issues']),
        ),
      'me_card' => PayloadMeCard(
          name: j['name'] as String?,
          tel: j['tel'] as String?,
          email: j['email'] as String?,
          url: j['url'] as String?,
        ),
      'crypto' => PayloadCrypto(
          scheme: _str(j['scheme']),
          address: _str(j['address']),
          amount: j['amount'] as String?,
        ),
      'v_card' => PayloadVCard(_str(j['raw'])),
      'v_event' => PayloadVEvent(_str(j['raw'])),
      'text' => const PayloadText(),
      _ => PayloadUnknown(kind, j),
    };
  }
}

class PayloadUrl implements Payload {
  @override
  String get kind => 'url';
  final String url;
  const PayloadUrl(this.url);
}

class PayloadWifi implements Payload {
  @override
  String get kind => 'wifi';
  final String ssid, security;
  final String? password;
  final bool hidden;
  const PayloadWifi(
      {required this.ssid,
      required this.security,
      required this.password,
      required this.hidden});
}

class PayloadEmail implements Payload {
  @override
  String get kind => 'email';
  final String to;
  final String? subject, body;
  const PayloadEmail({required this.to, required this.subject, required this.body});
}

class PayloadSms implements Payload {
  @override
  String get kind => 'sms';
  final String number;
  final String? body;
  const PayloadSms({required this.number, required this.body});
}

class PayloadTel implements Payload {
  @override
  String get kind => 'tel';
  final String number;
  const PayloadTel(this.number);
}

class PayloadGeo implements Payload {
  @override
  String get kind => 'geo';
  final double lat, lon;
  const PayloadGeo(this.lat, this.lon);
}

class PayloadGs1 implements Payload {
  @override
  String get kind => 'gs1';
  final List<Gs1Element> elements;
  final String? gtin;
  final bool conformant;
  final List<String> issues;
  const PayloadGs1(
      {required this.elements,
      required this.gtin,
      required this.conformant,
      required this.issues});
}

class PayloadGs1DigitalLink implements Payload {
  @override
  String get kind => 'gs1_digital_link';
  final String url;
  final List<Gs1Element> elements;
  final String? gtin;
  final bool conformant;
  final List<String> issues;
  const PayloadGs1DigitalLink(
      {required this.url,
      required this.elements,
      required this.gtin,
      required this.conformant,
      required this.issues});
}

class PayloadMeCard implements Payload {
  @override
  String get kind => 'me_card';
  final String? name, tel, email, url;
  const PayloadMeCard(
      {required this.name,
      required this.tel,
      required this.email,
      required this.url});
}

class PayloadCrypto implements Payload {
  @override
  String get kind => 'crypto';
  final String scheme, address;
  final String? amount;
  const PayloadCrypto(
      {required this.scheme, required this.address, required this.amount});
}

class PayloadVCard implements Payload {
  @override
  String get kind => 'v_card';
  final String raw;
  const PayloadVCard(this.raw);
}

class PayloadVEvent implements Payload {
  @override
  String get kind => 'v_event';
  final String raw;
  const PayloadVEvent(this.raw);
}

class PayloadText implements Payload {
  @override
  String get kind => 'text';
  const PayloadText();
}

/// A `kind` this binding version doesn't model — keeps the raw map so callers
/// can still read it. Forward-compatible by construction.
class PayloadUnknown implements Payload {
  @override
  final String kind;
  final Map<String, dynamic> raw;
  const PayloadUnknown(this.kind, this.raw);
}

// ─────────────────────────── Detection ───────────────────────────

class Detection {
  final Symbology symbology;
  final DecodedContent content;
  final Payload payload;
  final List<Point>? corners;
  final QrMeta meta;
  final List<EngineKind> engines;
  const Detection(this.symbology, this.content, this.payload, this.corners,
      this.meta, this.engines);

  factory Detection.fromJson(Map<String, dynamic> j) => Detection(
        Symbology.from(j['symbology']),
        DecodedContent.fromJson(_obj(j['content'])),
        Payload.fromJson(_obj(j['payload'])),
        j['corners'] is List
            ? (j['corners'] as List).map((c) => Point.fromJson(_obj(c))).toList()
            : null,
        QrMeta.fromJson(_obj(j['meta'])),
        (j['engines'] as List? ?? const []).map(EngineKind.from).toList(),
      );
}

// ─────────────────────────── Score ───────────────────────────

class AxisScore {
  final StressAxis axis;
  final int passed, total;

  /// Wire label of the first failed cell ("blur 2.5", "glare", "128px"…) —
  /// null when every cell passed.
  final String? failedAt;
  const AxisScore(this.axis, this.passed, this.total, this.failedAt);
  factory AxisScore.fromJson(Map<String, dynamic> j) => AxisScore(
      StressAxis.from(j['axis']),
      _int(j['passed']) ?? 0,
      _int(j['total']) ?? 0,
      j['failed_at'] as String?);
}

class StructuralReport {
  // 0.0-1.0 per corner — int truncation would collapse any partial
  // integrity (e.g. 0.96) to 0.
  final List<double> finderIntegrity; // [a, b, c]
  final bool quietZoneOk;
  const StructuralReport(this.finderIntegrity, this.quietZoneOk);
  factory StructuralReport.fromJson(Map<String, dynamic> j) => StructuralReport(
        (j['finder_integrity'] as List? ?? const [])
            .map(_double)
            .toList(),
        j['quiet_zone_ok'] == true,
      );
}

class UecReport {
  // 0.0-1.0 (1.0 = pristine) — int truncation read every real margin as 0.
  final double margin;
  final LetterGrade grade;
  final int worstBlockErrors, worstBlockCapacity;
  const UecReport(
      this.margin, this.grade, this.worstBlockErrors, this.worstBlockCapacity);
  factory UecReport.fromJson(Map<String, dynamic> j) => UecReport(
        _double(j['margin']),
        LetterGrade.from(j['grade']),
        _int(j['worst_block_errors']) ?? 0,
        _int(j['worst_block_capacity']) ?? 0,
      );
}

class IsoParameter {
  final double value;
  final LetterGrade grade;
  const IsoParameter(this.value, this.grade);
  factory IsoParameter.fromJson(Map<String, dynamic> j) =>
      IsoParameter(_double(j['value']), LetterGrade.from(j['grade']));
}

class Iso15415Report {
  final IsoParameter symbolContrast;
  final IsoParameter modulation;
  final IsoParameter axialNonuniformity;
  final IsoParameter fixedPatternDamage;
  final IsoParameter? unusedErrorCorrection;
  final LetterGrade overall;
  const Iso15415Report(
      this.symbolContrast,
      this.modulation,
      this.axialNonuniformity,
      this.fixedPatternDamage,
      this.unusedErrorCorrection,
      this.overall);

  factory Iso15415Report.fromJson(Map<String, dynamic> j) => Iso15415Report(
        IsoParameter.fromJson(_obj(j['symbol_contrast'])),
        IsoParameter.fromJson(_obj(j['modulation'])),
        IsoParameter.fromJson(_obj(j['axial_nonuniformity'])),
        IsoParameter.fromJson(_obj(j['fixed_pattern_damage'])),
        j['unused_error_correction'] == null
            ? null
            : IsoParameter.fromJson(_obj(j['unused_error_correction'])),
        LetterGrade.from(j['overall']),
      );
}

class Score {
  final int value;
  final Grade grade;
  final List<AxisScore> axes;
  final StructuralReport? structural;
  final UecReport? uec;
  final Iso15415Report? iso15415;
  const Score(this.value, this.grade, this.axes, this.structural, this.uec,
      this.iso15415);

  factory Score.fromJson(Map<String, dynamic> j) => Score(
        _int(j['value']) ?? 0,
        Grade.from(j['grade']),
        _objList(j['axes']).map(AxisScore.fromJson).toList(),
        j['structural'] == null
            ? null
            : StructuralReport.fromJson(_obj(j['structural'])),
        j['uec'] == null ? null : UecReport.fromJson(_obj(j['uec'])),
        j['iso15415'] == null
            ? null
            : Iso15415Report.fromJson(_obj(j['iso15415'])),
      );
}

// ─────────────────────────── Hint (tagged union) ───────────────────────────

/// A machine-actionable improvement hint. Unknown hints decode to [HintUnknown].
sealed class Hint {
  String get hint;

  factory Hint.fromJson(Map<String, dynamic> j) {
    final h = _str(j['hint']);
    return switch (h) {
      'raise_error_correction' => HintRaiseErrorCorrection(EcLevel.from(j['current'])),
      'increase_contrast' => const HintIncreaseContrast(),
      'enlarge_modules' => const HintEnlargeModules(),
      'fix_finder_pattern' => HintFixFinderPattern(_int(j['corner']) ?? 0),
      'restore_quiet_zone' => const HintRestoreQuietZone(),
      'reduce_art_texture' => const HintReduceArtTexture(),
      'low_correction_margin' => HintLowCorrectionMargin(
          errors: _int(j['errors']) ?? 0, capacity: _int(j['capacity']) ?? 0),
      _ => HintUnknown(h, j),
    };
  }
}

class HintRaiseErrorCorrection implements Hint {
  @override
  String get hint => 'raise_error_correction';
  final EcLevel current;
  const HintRaiseErrorCorrection(this.current);
}

class HintIncreaseContrast implements Hint {
  @override
  String get hint => 'increase_contrast';
  const HintIncreaseContrast();
}

class HintEnlargeModules implements Hint {
  @override
  String get hint => 'enlarge_modules';
  const HintEnlargeModules();
}

class HintFixFinderPattern implements Hint {
  @override
  String get hint => 'fix_finder_pattern';
  final int corner;
  const HintFixFinderPattern(this.corner);
}

class HintRestoreQuietZone implements Hint {
  @override
  String get hint => 'restore_quiet_zone';
  const HintRestoreQuietZone();
}

class HintReduceArtTexture implements Hint {
  @override
  String get hint => 'reduce_art_texture';
  const HintReduceArtTexture();
}

class HintLowCorrectionMargin implements Hint {
  @override
  String get hint => 'low_correction_margin';
  final int errors, capacity;
  const HintLowCorrectionMargin({required this.errors, required this.capacity});
}

class HintUnknown implements Hint {
  @override
  final String hint;
  final Map<String, dynamic> raw;
  const HintUnknown(this.hint, this.raw);
}

// ─────────────────────────── trace + versions ───────────────────────────

class StageTrace {
  final String stage;
  final int transformsTried, detectionsFound;
  final double ms;
  const StageTrace(
      this.stage, this.transformsTried, this.ms, this.detectionsFound);
  factory StageTrace.fromJson(Map<String, dynamic> j) => StageTrace(
        _str(j['stage']),
        _int(j['transforms_tried']) ?? 0,
        _double(j['ms']),
        _int(j['detections_found']) ?? 0,
      );
}

class PipelineTrace {
  final List<StageTrace> stages;
  final int enginePanics;
  final double totalMs;
  const PipelineTrace(this.stages, this.enginePanics, this.totalMs);
  factory PipelineTrace.fromJson(Map<String, dynamic> j) => PipelineTrace(
        _objList(j['stages']).map(StageTrace.fromJson).toList(),
        _int(j['engine_panics']) ?? 0,
        _double(j['total_ms']),
      );
}

class Versions {
  final String scanner;
  final int pipeline, scoreContract;
  const Versions(this.scanner, this.pipeline, this.scoreContract);
  factory Versions.fromJson(Map<String, dynamic> j) => Versions(
        _str(j['scanner']),
        _int(j['pipeline']) ?? 0,
        _int(j['score_contract']) ?? 0,
      );
}

// ─────────────────────────── ScanReport ───────────────────────────

class ScanReport {
  /// Empty = no QR found (a valid outcome, not an error).
  final List<Detection> detections;

  /// null in the `frame` profile.
  final Score? score;
  final List<Hint> hints;
  final PipelineTrace trace;
  final Versions versions;

  /// The original decoded JSON map — escape hatch for any field this typed
  /// view does not yet model.
  final Map<String, dynamic> raw;

  const ScanReport(this.detections, this.score, this.hints, this.trace,
      this.versions, this.raw);

  factory ScanReport.fromJson(Map<String, dynamic> j) => ScanReport(
        _objList(j['detections']).map(Detection.fromJson).toList(),
        j['score'] == null ? null : Score.fromJson(_obj(j['score'])),
        _objList(j['hints']).map(Hint.fromJson).toList(),
        PipelineTrace.fromJson(_obj(j['trace'])),
        Versions.fromJson(_obj(j['versions'])),
        j,
      );

  /// Parse a raw JSON string (what the native bindings return).
  factory ScanReport.parse(String jsonString) =>
      ScanReport.fromJson(jsonDecode(jsonString) as Map<String, dynamic>);
}
