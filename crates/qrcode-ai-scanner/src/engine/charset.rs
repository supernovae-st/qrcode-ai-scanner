//! Charset resolution for raw decoded bytes WITHOUT an ECI declaration —
//! the heuristic `engine::content` applies when no designator is present,
//! never per-engine — plus what each legacy wire label decodes.
//!
//! Order: strict UTF-8 → isolated-byte Latin-1 short-circuit → Shift-JIS
//! (error-free decode) → windows-1252 (never fails). Decoders never sniff a
//! byte-order mark: a payload starting `FF FE` is not UTF-16 because it
//! looks like one.
//!
//! The short-circuit is the load-bearing part: most Latin-1 accented bytes
//! (0x81-0x9F · 0xE0-0xFC) followed by an ASCII letter form a VALID
//! Shift-JIS kanji pair, so "SJIS decodes error-free" alone garbles the
//! dominant legacy European class ("Québec" → "Qu饕ec", review-proven).
//! Real Shift-JIS text has RUNS of non-ASCII bytes (kanji pairs, katakana
//! sequences); Latin-1 European text has accented bytes ISOLATED between
//! ASCII. Known residual ambiguity: a single isolated half-width katakana
//! between ASCII letters now reads as Latin-1 — inherent, documented.

use crate::report::Charset;

/// Every non-ASCII byte sits alone between ASCII neighbours (or string
/// edges) — the Latin-1-not-SJIS signature.
fn all_non_ascii_isolated(raw: &[u8]) -> bool {
    raw.iter().enumerate().all(|(i, b)| {
        b.is_ascii()
            || ((i == 0 || raw[i - 1].is_ascii()) && (i + 1 == raw.len() || raw[i + 1].is_ascii()))
    })
}

/// Resolve raw payload bytes into text + the charset that produced it.
pub(crate) fn resolve(raw: &[u8]) -> (String, Charset) {
    if let Ok(s) = std::str::from_utf8(raw) {
        return (s.to_owned(), Charset::Utf8);
    }
    if all_non_ascii_isolated(raw) {
        return (windows_1252(raw), Charset::Latin1);
    }
    if let Some(text) =
        encoding_rs::SHIFT_JIS.decode_without_bom_handling_and_without_replacement(raw)
    {
        return (text.into_owned(), Charset::ShiftJis);
    }
    (windows_1252(raw), Charset::Latin1)
}

/// The `latin1` label's decoder: Windows-1252, total over every byte.
fn windows_1252(raw: &[u8]) -> String {
    encoding_rs::WINDOWS_1252
        .decode_without_bom_handling(raw)
        .0
        .into_owned()
}

/// Decode `raw` the way a decoder label promises — `None` when the label's
/// decoder rejects a byte, or the label names no decoder (`eci`, `unknown`,
/// a newer scanner's label). `Latin1` means what the heuristic decodes
/// under it: the Windows-1252 superset, total.
pub(crate) fn decode_legacy(raw: &[u8], label: Charset) -> Option<String> {
    let strict = |encoding: &'static encoding_rs::Encoding| {
        encoding
            .decode_without_bom_handling_and_without_replacement(raw)
            .map(std::borrow::Cow::into_owned)
    };
    match label {
        Charset::Utf8 => std::str::from_utf8(raw).ok().map(str::to_owned),
        Charset::ShiftJis => strict(encoding_rs::SHIFT_JIS),
        Charset::Latin1 => Some(windows_1252(raw)),
        Charset::Gb18030 => strict(encoding_rs::GB18030),
        Charset::Eci | Charset::Unknown | Charset::Unrecognized => None,
    }
}

/// ISO/IEC 8859-1 proper: byte n is U+00nn, C1 controls included. Total and
/// injective — so also the scanner's lossless rendering of bytes it cannot
/// read faithfully (distinct bytes never collapse into one text).
pub(crate) fn iso_8859_1(raw: &[u8]) -> String {
    raw.iter().map(|&b| char::from(b)).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn windows_1252(s: &str) -> Vec<u8> {
        let (bytes, _, had_errors) = encoding_rs::WINDOWS_1252.encode(s);
        assert!(!had_errors);
        bytes.into_owned()
    }

    #[test]
    fn latin1_accented_words_resolve_as_latin1_not_garbled_sjis() {
        // review-proven garble class: accented byte + ASCII letter forms a
        // valid SJIS kanji pair ("Québec" read "Qu饕ec" pre-fix)
        for truth in ["Québec", "señor", "crème brûlée", "café au lait", "Éé"] {
            let raw = windows_1252(truth);
            let (text, charset) = resolve(&raw);
            assert_eq!(text, truth);
            assert_eq!(charset, Charset::Latin1, "{truth}");
        }
    }

    #[test]
    fn real_shift_jis_runs_still_resolve_as_sjis() {
        // kanji pairs = consecutive non-ASCII bytes — not isolated
        let (raw, _, had_errors) = encoding_rs::SHIFT_JIS.encode("日本語テスト");
        assert!(!had_errors);
        let (text, charset) = resolve(&raw);
        assert_eq!(text, "日本語テスト");
        assert_eq!(charset, Charset::ShiftJis);
        // mixed ASCII + kanji run keeps the SJIS reading too
        let (raw, _, _) = encoding_rs::SHIFT_JIS.encode("QR: 漢字");
        let (text, charset) = resolve(&raw);
        assert_eq!(text, "QR: 漢字");
        assert_eq!(charset, Charset::ShiftJis);
    }

    #[test]
    fn utf8_always_wins() {
        let (text, charset) = resolve("déjà 日本".as_bytes());
        assert_eq!(text, "déjà 日本");
        assert_eq!(charset, Charset::Utf8);
    }

    #[test]
    fn byte_order_marks_are_bytes_not_encoding_switches() {
        // FF FE looks like a UTF-16LE BOM: a sniffing decoder read this as
        // "A" and labelled it shift_jis. The bytes are not UTF-16 because
        // they start like it — Windows-1252 over every byte, as labelled.
        let (text, charset) = resolve(&[0xFF, 0xFE, 0x41, 0x00]);
        assert_eq!(text, "ÿþA\u{0}");
        assert_eq!(charset, Charset::Latin1);
        assert_eq!(
            decode_legacy(&[0xFF, 0xFE, 0x41, 0x00], charset).as_deref(),
            Some("ÿþA\u{0}")
        );
    }

    #[test]
    fn legacy_labels_decode_what_they_promise() {
        assert_eq!(
            decode_legacy(&[0xA1], Charset::Latin1).as_deref(),
            Some("¡")
        );
        // the latin1 label is the Windows-1252 superset: 0x80 is '€'
        assert_eq!(
            decode_legacy(&[0x80], Charset::Latin1).as_deref(),
            Some("€")
        );
        assert_eq!(decode_legacy(&[0xA1], Charset::Utf8), None);
        assert_eq!(
            decode_legacy(&[0x82, 0xB1], Charset::ShiftJis).as_deref(),
            Some("こ")
        );
        assert_eq!(decode_legacy(&[0xFF], Charset::ShiftJis), None);
        assert_eq!(
            decode_legacy(&[0xD6, 0xD0, 0xCE, 0xC4], Charset::Gb18030).as_deref(),
            Some("中文")
        );
        assert_eq!(decode_legacy(&[0xB0, 0xFF], Charset::Gb18030), None);
        for label in [Charset::Eci, Charset::Unknown, Charset::Unrecognized] {
            assert_eq!(
                decode_legacy(b"ok", label),
                None,
                "{label:?} has no decoder"
            );
        }
        // ISO/IEC 8859-1 proper keeps C1 controls — and is injective
        assert_eq!(iso_8859_1(&[0x35, 0x80, 0xFF]), "5\u{80}ÿ");
    }

    #[test]
    fn invalid_everywhere_falls_back_to_latin1_lossy() {
        // 0xFF is outside the SJIS lead ranges → SJIS errors; consecutive
        // non-ASCII → not isolated → the final lossy Latin-1 fallback
        let (_, charset) = resolve(&[0xFF, 0xFF]);
        assert_eq!(charset, Charset::Latin1);
    }

    /// Directly pin the isolation predicate — the heuristic's boolean is the
    /// load-bearing part and every one of the neighbour-index mutations lives
    /// here. Column-exact targets on line 22:
    /// - `22:34 -→+` (`raw[i-1]`→`raw[i+1]`), `22:57 +→*` (`i+1`→`i*1` in the
    ///   `i + 1 == len` edge guard) and `22:61 ==→!=` all read `raw[i+1]` at the
    ///   LAST index when the trailing byte is a reached isolated non-ASCII →
    ///   out-of-bounds panic. The original returns `true` here.
    /// - `22:51 &&→||` turns "both neighbours ASCII" into "either", so an
    ///   interior adjacent-non-ASCII pair (which the original rejects) is
    ///   wrongly accepted.
    #[test]
    fn all_non_ascii_isolated_neighbour_logic_is_exact() {
        // isolated accent that is the FINAL byte, all prior ASCII: the only
        // input that REACHES the last-index `raw[i+1]` read on a true prefix.
        // original → true; the -/+/*/== index mutants index raw[len] → panic.
        assert!(all_non_ascii_isolated(&[0x41, 0xE9])); // "A" + é
        // and the same fact through the public surface (Windows-1252 → "Aé").
        assert_eq!(resolve(&[0x41, 0xE9]), ("Aé".to_owned(), Charset::Latin1));

        // interior ADJACENT non-ASCII pair (é ê between ASCII): the left
        // neighbour of ê is non-ASCII → the `&&` rejects it. `&&→||` would
        // accept it because each byte still has ONE ASCII neighbour.
        assert!(!all_non_ascii_isolated(&[0x41, 0xE9, 0xEA, 0x41])); // A é ê A
        // a truly isolated interior accent stays accepted (control).
        assert!(all_non_ascii_isolated(&[0x41, 0xE9, 0x41])); // A é A
    }
}
