//! QR content observations and their ONE interpretation (ISO/IEC 18004
//! §7.3.5 · §7.4.2 · §7.4.8).
//!
//! An observation is payload bytes plus what its source could see of the
//! ECI declarations and of the FNC1 position. Two reads are one physical
//! content when their bytes are identical and their accounts agree
//! ([`ContentKey::admits`]); [`read`] then turns the content into text
//! exactly once:
//!
//! 1. Located designators govern their bytes until the next designator or
//!    the end of data (§7.4.2.3). Every byte before the first designator is
//!    ISO/IEC 8859-1 whatever mode encoded it (the default interpretation,
//!    §7.3.5) — text is a function of the bytes and the designators alone.
//! 2. A payload without any designator keeps the legacy charset heuristic
//!    ([`charset::resolve`]) and its label.
//! 3. What cannot be decoded is marked, never transcribed nor guessed: one
//!    U+FFFD per maximal ill-formed subpart inside a supported set (WHATWG),
//!    one per byte under ECI 899 or a designator outside the decode table.
//! 4. A designator an engine signalled without locating it leaves the text
//!    unverified: its bytes are marked unknown.

use super::{RawDetection, charset};
use crate::report::Charset;

/// One ECI designator: its six-digit assignment number and the payload
/// offset from which it governs the bytes (§7.4.2.2-7.4.2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EciMark {
    /// Assignment number, 0..=999999 (the parser refuses anything else).
    pub(crate) value: u32,
    /// Offset into the payload bytes where the declaration takes effect.
    pub(crate) start: usize,
}

/// The FNC1 mode a read observed (ISO 18004 §7.4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fnc1 {
    /// No FNC1 mode indicator — observed: rxing's AIM modifier `]Q0`-`]Q2`,
    /// a parsed stream, or an rqrr decode (rqrr decodes neither FNC1 mode).
    None,
    /// FNC1 in first position — GS1 data (`]Q3`/`]Q4`).
    First,
    /// FNC1 in second position — an AIM application, never GS1
    /// (`]Q5`/`]Q6`); its application indicator leads the payload bytes.
    Second,
    /// The position is unknown: a symbology outside the QR family, or an
    /// rxing result whose symbology identifier does not parse.
    Unobserved,
}

impl Fnc1 {
    /// Observed positions agree only with themselves — a plain symbol, a
    /// GS1 one and an AIM one are three contents even over equal bytes;
    /// an unknown position agrees with any.
    fn agrees_with(self, other: Self) -> bool {
        self == other || self == Self::Unobserved || other == Self::Unobserved
    }
}

/// What one read could see of the ECI declarations of its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EciEvidence {
    /// Nothing observable: a symbology outside the QR family, or a stream
    /// re-derivation that failed or told another story than the engine's
    /// bytes (e.g. the unmirrored read of a mirrored symbol).
    Unobserved,
    /// No designator: stated by the engine (AIM modifier `]Q0`/`]Q1`/
    /// `]Q3`/`]Q5`) or observed in the parsed stream.
    NoEci,
    /// The engine stated a designator is present (`]Q2`/`]Q4`/`]Q6`) but
    /// exposes neither its number nor its position.
    Signalled,
    /// The designators, parsed by the scanner from RS-corrected data
    /// codewords, in stream order (never empty).
    Located(Vec<EciMark>),
}

impl EciEvidence {
    /// The account of a parsed stream: its designators, or their observed
    /// absence.
    pub(crate) fn from_marks(marks: Vec<EciMark>) -> Self {
        if marks.is_empty() {
            Self::NoEci
        } else {
            Self::Located(marks)
        }
    }

    /// The compatibility lattice: unobserved ~ anything · no-ECI ~ no-ECI ·
    /// signalled ~ signalled | located · located(a) ~ located(a) | signalled.
    /// Everything else is a different content (an unmarked `A1` and an
    /// ECI-4 `A1` stay two).
    ///
    /// Two reads whose observed accounts disagree never merge. The converse
    /// is weaker, and that is the lattice's residual ambiguity: a read
    /// without located evidence (unobserved, signalled) agrees with several
    /// contents of equal bytes. The ladder lets geometry decide where the
    /// read has corners (it joins the admitting detection they overlap),
    /// else the first in attempt order; two signalled-only reads of equal
    /// bytes under different, never exposed designators (two rxing-only
    /// symbols, ECI 4 and ECI 7, no geometry) still reconcile into one, and
    /// a single located read then decides the text of both.
    pub(crate) fn agrees_with(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Unobserved, _)
            | (_, Self::Unobserved)
            | (Self::NoEci, Self::NoEci)
            | (Self::Signalled, Self::Signalled | Self::Located(_))
            | (Self::Located(_), Self::Signalled) => true,
            (Self::Located(a), Self::Located(b)) => a == b,
            (Self::NoEci, _) | (_, Self::NoEci) => false,
        }
    }

    fn specificity(&self) -> u8 {
        match self {
            Self::Unobserved => 0,
            Self::NoEci | Self::Signalled => 1,
            Self::Located(_) => 2,
        }
    }
}

/// What one detection's content IS: the reconciliation identity, and what
/// score stress cells and alpha probes compare an engine hit against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContentKey {
    /// Original payload bytes, designators excluded.
    pub(crate) raw: Vec<u8>,
    /// The most specific ECI account any admitted read gave.
    pub(crate) eci: EciEvidence,
    /// The FNC1 position the admitted reads observed.
    pub(crate) fnc1: Fnc1,
}

impl ContentKey {
    /// Whether an engine hit reads this same content: identical bytes,
    /// agreeing ECI accounts, no FNC1 position conflict. Symbology is the
    /// caller's filter.
    pub(crate) fn admits(&self, hit: &RawDetection) -> bool {
        self.raw == hit.raw && self.eci.agrees_with(&hit.eci) && self.fnc1.agrees_with(hit.fnc1)
    }

    /// Join an admitted read's account: the most specific one stays
    /// (located over signalled, no-ECI over unobserved, an observed FNC1
    /// position over an unknown one; the first on a tie).
    pub(crate) fn join(&mut self, eci: EciEvidence, fnc1: Fnc1) {
        if eci.specificity() > self.eci.specificity() {
            self.eci = eci;
        }
        if self.fnc1 == Fnc1::Unobserved {
            self.fnc1 = fnc1;
        }
    }
}

/// How a reading may be labelled — the account the three-label wire
/// contract cannot carry yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Label {
    /// The legacy label whose decoder reproduces the text from the bytes
    /// exactly (always the case for the heuristic's own choice).
    Legacy(Charset),
    /// Located declarations decoded without a mark, under no single legacy
    /// label.
    Eci,
    /// Something is marked or unverified: a replacement, a designator that
    /// is binary or outside the decode table, a signalled-only declaration.
    Unknown,
}

/// The text of one content and how it may be labelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reading {
    /// The text; U+FFFD marks what has no reading.
    pub(crate) text: String,
    /// The label decision.
    pub(crate) label: Label,
}

impl Reading {
    /// The label published while the wire contract has only the three
    /// legacy labels: the legacy label when one is faithful, else `utf8` as
    /// a placeholder — never `latin1`/`shift_jis`, which would claim a
    /// legacy decoding for content that has none (`label` keeps the full account
    /// until `eci`/`unknown` exist on the wire).
    pub(crate) fn published(&self) -> Charset {
        match self.label {
            Label::Legacy(label) => label,
            Label::Eci | Label::Unknown => Charset::Utf8,
        }
    }
}

/// Interpret one content — THE content rule of the crate (module doc).
pub(crate) fn read(raw: &[u8], eci: &EciEvidence) -> Reading {
    match eci {
        EciEvidence::Located(marks) => read_located(raw, marks),
        EciEvidence::Signalled => read_signalled(raw),
        EciEvidence::NoEci | EciEvidence::Unobserved => read_unmarked(raw),
    }
}

/// No designator observed: the legacy heuristic and its label. (A mode-
/// implied reading of Hanzi or mixed byte+Kanji payloads is still an open
/// contract question; this is the one place it would land.)
fn read_unmarked(raw: &[u8]) -> Reading {
    let (text, label) = charset::resolve(raw);
    Reading {
        text,
        label: Label::Legacy(label),
    }
}

/// A designator was signalled but not located: which bytes it governs and
/// under which set is unknown, so is the text. Conservative until the
/// publication rule for signalled-only content is final: every byte marked.
fn read_signalled(raw: &[u8]) -> Reading {
    Reading {
        text: replacement(raw.len()),
        label: Label::Unknown,
    }
}

fn read_located(raw: &[u8], marks: &[EciMark]) -> Reading {
    // designator boundaries, clamped monotone into the payload (the parser
    // never produces anything else — this only keeps the function total)
    let mut starts = Vec::with_capacity(marks.len());
    let mut previous = 0;
    for mark in marks {
        previous = mark.start.clamp(previous, raw.len());
        starts.push(previous);
    }
    let first = starts.first().copied().unwrap_or(raw.len());
    let mut text = charset::iso_8859_1(raw.get(..first).unwrap_or_default());
    let mut marked = false;
    let ends = starts.iter().skip(1).copied().chain([raw.len()]);
    for ((mark, &start), end) in marks.iter().zip(&starts).zip(ends) {
        let bytes = raw.get(start..end).unwrap_or_default();
        match eci_set(mark.value) {
            Some(set) => {
                let (part, replaced) = decode(bytes, set);
                text.push_str(&part);
                marked |= replaced;
            }
            None => text.push_str(&replacement(bytes.len())),
        }
    }
    // any listed designator without a set — even one governing no byte —
    // means the content is not fully readable as declared
    let label = if marked || marks.iter().any(|mark| eci_set(mark.value).is_none()) {
        Label::Unknown
    } else {
        legacy_family(marks)
            .filter(|&label| charset::decode_legacy(raw, label).as_deref() == Some(text.as_str()))
            .map_or(Label::Eci, Label::Legacy)
    };
    Reading { text, label }
}

/// The legacy label every listed designator belongs to — `utf8`: 26
/// (UTF-8), 27 (ASCII) · `shift_jis`: 20 · `latin1`: 1, 3 (ISO/IEC
/// 8859-1), 23 (Windows-1252). `None` when they span several families or
/// none.
fn legacy_family(marks: &[EciMark]) -> Option<Charset> {
    let mut family = None;
    for mark in marks {
        let own = match mark.value {
            26 | 27 => Charset::Utf8,
            20 => Charset::ShiftJis,
            1 | 3 | 23 => Charset::Latin1,
            _ => return None,
        };
        if family.is_some_and(|shared| shared != own) {
            return None;
        }
        family = Some(own);
    }
    family
}

const REPLACEMENT: char = '\u{FFFD}';

fn replacement(count: usize) -> String {
    std::iter::repeat_n(REPLACEMENT, count).collect()
}

/// A character set of the decode table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EciSet {
    /// ISO/IEC 8859-1 — byte n is U+00nn, C1 controls included.
    Latin1,
    /// ISO/IEC 8859-9, in-crate (no WHATWG decoder exists for it).
    Iso8859_9,
    /// ISO/IEC 8859-11, in-crate (no WHATWG decoder exists for it).
    Iso8859_11,
    /// Decoded by `encoding_rs` exactly as registered.
    Whatwg(&'static encoding_rs::Encoding),
    /// US-ASCII: bytes from 0x80 have no reading.
    Ascii,
    /// UTF-32 (ECI 34 big endian, ECI 35 little endian), in-crate.
    Utf32 {
        /// Byte order of each code unit.
        big_endian: bool,
    },
}

/// The normative decode table. `None`: no character set — ECI 899 (binary,
/// never text), 0/2 (CP437), 14, 19, 170 and every other number.
fn eci_set(value: u32) -> Option<EciSet> {
    use encoding_rs as e;
    Some(match value {
        1 | 3 => EciSet::Latin1,
        4 => EciSet::Whatwg(e::ISO_8859_2),
        5 => EciSet::Whatwg(e::ISO_8859_3),
        6 => EciSet::Whatwg(e::ISO_8859_4),
        7 => EciSet::Whatwg(e::ISO_8859_5),
        8 => EciSet::Whatwg(e::ISO_8859_6),
        9 => EciSet::Whatwg(e::ISO_8859_7),
        10 => EciSet::Whatwg(e::ISO_8859_8),
        11 => EciSet::Iso8859_9,
        12 => EciSet::Whatwg(e::ISO_8859_10),
        13 => EciSet::Iso8859_11,
        15 => EciSet::Whatwg(e::ISO_8859_13),
        16 => EciSet::Whatwg(e::ISO_8859_14),
        17 => EciSet::Whatwg(e::ISO_8859_15),
        18 => EciSet::Whatwg(e::ISO_8859_16),
        20 => EciSet::Whatwg(e::SHIFT_JIS),
        21 => EciSet::Whatwg(e::WINDOWS_1250),
        22 => EciSet::Whatwg(e::WINDOWS_1251),
        23 => EciSet::Whatwg(e::WINDOWS_1252),
        24 => EciSet::Whatwg(e::WINDOWS_1256),
        25 => EciSet::Whatwg(e::UTF_16BE),
        26 => EciSet::Whatwg(e::UTF_8),
        27 => EciSet::Ascii,
        28 => EciSet::Whatwg(e::BIG5),
        // GB 2312 · GBK · GB 18030: the gb18030 decoder reads all three
        29 | 31 | 32 => EciSet::Whatwg(e::GB18030),
        30 => EciSet::Whatwg(e::EUC_KR),
        33 => EciSet::Whatwg(e::UTF_16LE),
        34 => EciSet::Utf32 { big_endian: true },
        35 => EciSet::Utf32 { big_endian: false },
        _ => return None,
    })
}

/// Decode `bytes` under `set`, marking what has no reading — one U+FFFD per
/// maximal ill-formed subpart (WHATWG), no BOM sniffing. Returns whether
/// anything was marked.
fn decode(bytes: &[u8], set: EciSet) -> (String, bool) {
    match set {
        EciSet::Latin1 => (charset::iso_8859_1(bytes), false),
        EciSet::Iso8859_9 => (bytes.iter().map(|&b| iso_8859_9(b)).collect(), false),
        EciSet::Iso8859_11 => mark_each(bytes, iso_8859_11),
        EciSet::Ascii => mark_each(bytes, |b| b.is_ascii().then_some(char::from(b))),
        EciSet::Whatwg(encoding) => {
            let (text, marked) = encoding.decode_without_bom_handling(bytes);
            (text.into_owned(), marked)
        }
        EciSet::Utf32 { big_endian } => {
            let units = bytes.chunks_exact(4);
            // a truncated final unit is one ill-formed subpart
            let truncated = !units.remainder().is_empty();
            let mut marked = truncated;
            let mut text = String::with_capacity(bytes.len() / 4 + 1);
            for unit in units {
                let word = <[u8; 4]>::try_from(unit).map_or(u32::MAX, |unit| {
                    if big_endian {
                        u32::from_be_bytes(unit)
                    } else {
                        u32::from_le_bytes(unit)
                    }
                });
                if let Some(c) = char::from_u32(word) {
                    text.push(c);
                } else {
                    marked = true;
                    text.push(REPLACEMENT);
                }
            }
            if truncated {
                text.push(REPLACEMENT);
            }
            (text, marked)
        }
    }
}

/// Decode a single-byte set byte by byte, marking unassigned bytes.
fn mark_each(bytes: &[u8], map: impl Fn(u8) -> Option<char>) -> (String, bool) {
    let mut marked = false;
    let text = bytes
        .iter()
        .map(|&b| {
            map(b).unwrap_or_else(|| {
                marked = true;
                REPLACEMENT
            })
        })
        .collect();
    (text, marked)
}

/// ISO/IEC 8859-9 (Latin-5): ISO/IEC 8859-1 with six Turkish letters.
fn iso_8859_9(byte: u8) -> char {
    match byte {
        0xD0 => 'Ğ',
        0xDD => 'İ',
        0xDE => 'Ş',
        0xF0 => 'ğ',
        0xFD => 'ı',
        0xFE => 'ş',
        _ => char::from(byte),
    }
}

/// ISO/IEC 8859-11 (Thai): 0x00-0xA0 as ISO/IEC 8859-1 (C1 controls,
/// no-break space); 0xA1-0xDA and 0xDF-0xFB are U+0E01-U+0E3A and
/// U+0E3F-U+0E5B; 0xDB-0xDE and 0xFC-0xFF are unassigned.
fn iso_8859_11(byte: u8) -> Option<char> {
    match byte {
        0x00..=0xA0 => Some(char::from(byte)),
        0xA1..=0xDA | 0xDF..=0xFB => char::from_u32(0x0E01 + u32::from(byte - 0xA1)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use proptest::prelude::*;

    use super::*;
    use crate::report::{EngineKind, Symbology};

    fn located(marks: &[(u32, usize)]) -> EciEvidence {
        EciEvidence::Located(
            marks
                .iter()
                .map(|&(value, start)| EciMark { value, start })
                .collect(),
        )
    }

    const KONNICHIWA_SJIS: [u8; 10] = [0x82, 0xB1, 0x82, 0xF1, 0x82, 0xC9, 0x82, 0xBF, 0x82, 0xCD];

    #[test]
    fn unmarked_payloads_keep_the_heuristic_and_its_label() {
        for evidence in [EciEvidence::NoEci, EciEvidence::Unobserved] {
            for raw in [
                &[0xA1][..],
                &[0x35, 0x80],
                "héllo → 🦋".as_bytes(),
                &KONNICHIWA_SJIS,
            ] {
                let (text, label) = charset::resolve(raw);
                let reading = read(raw, &evidence);
                assert_eq!(reading.text, text, "{evidence:?} {raw:?}");
                assert_eq!(reading.label, Label::Legacy(label));
                assert_eq!(reading.published(), label);
            }
        }
    }

    #[test]
    fn a_located_designator_decides_its_bytes() {
        // ECI 4 (ISO/IEC 8859-2): A1 is 'Ą' — designator 4 is in no legacy
        // family, so the honest label is `eci`
        let reading = read(&[0xA1], &located(&[(4, 0)]));
        assert_eq!(reading.text, "\u{104}");
        assert_eq!(reading.label, Label::Eci);
        // the three-label wire carries the utf8 placeholder meanwhile, never
        // the heuristic's latin1 for [A1]
        assert_eq!(reading.published(), Charset::Utf8);
        let privet = [0xBF, 0xE0, 0xD8, 0xD2, 0xD5, 0xE2];
        assert_eq!(read(&privet, &located(&[(7, 0)])).text, "Привет");
        // ASCII under ECI 4 still reads as declared: `eci`, not utf8
        assert_eq!(read(b"abc", &located(&[(4, 0)])).label, Label::Eci);
    }

    /// (raw, designators as (value, start), text, label)
    type LabelCase = (&'static [u8], &'static [(u32, usize)], &'static str, Label);

    #[test]
    fn the_legacy_label_requires_its_family_and_an_exact_reproduction() {
        let cases: [LabelCase; 7] = [
            (
                "\u{104}".as_bytes(),
                &[(26, 0)],
                "\u{104}",
                Label::Legacy(Charset::Utf8),
            ),
            (b"A", &[(27, 0)], "A", Label::Legacy(Charset::Utf8)),
            (
                &KONNICHIWA_SJIS,
                &[(20, 0)],
                "こんにちは",
                Label::Legacy(Charset::ShiftJis),
            ),
            (&[0xA1], &[(3, 0)], "¡", Label::Legacy(Charset::Latin1)),
            (&[0x80], &[(23, 0)], "€", Label::Legacy(Charset::Latin1)),
            // ISO/IEC 8859-1 keeps 0x80 a C1 control; the latin1 label's
            // decoder reads '€' — no reproduction, so `eci`
            (&[0x35, 0x80], &[(3, 0)], "5\u{80}", Label::Eci),
            // two families (latin1 + utf8): no single legacy label
            (b"a\xC3\xA9", &[(3, 0), (26, 1)], "aé", Label::Eci),
        ];
        for (raw, marks, text, label) in cases {
            let reading = read(raw, &located(marks));
            assert_eq!(reading.text, text, "{marks:?}");
            assert_eq!(reading.label, label, "{marks:?}");
        }
    }

    #[test]
    fn each_designator_governs_until_the_next() {
        let raw = [0xA1, 0xE2, 0x82, 0xAC];
        let reading = read(&raw, &located(&[(4, 0), (26, 1)]));
        assert_eq!(reading.text, "\u{104}€");
        assert_eq!(reading.label, Label::Eci);
    }

    #[test]
    fn every_byte_before_the_first_designator_is_iso_8859_1() {
        let reading = read(b"12345\xA1", &located(&[(4, 5)]));
        assert_eq!(reading.text, "12345\u{104}");
        // Kanji pairs there too: transmission-faithful, never Shift JIS
        let mut raw = KONNICHIWA_SJIS[..2].to_vec();
        raw.push(b'a');
        let reading = read(&raw, &located(&[(26, 2)]));
        assert_eq!(reading.text, "\u{82}±a");
        assert_eq!(reading.label, Label::Eci, "decoded without a mark");
    }

    #[test]
    fn undecodable_bytes_are_marked_never_transcribed() {
        // inside a supported set: one U+FFFD per maximal ill-formed subpart
        let reading = read(&[0xA1], &located(&[(26, 0)]));
        assert_eq!(reading.text, "\u{FFFD}");
        assert_eq!(reading.label, Label::Unknown);
        let mut raw = "é".as_bytes().to_vec();
        raw.extend_from_slice(&KONNICHIWA_SJIS);
        let reading = read(&raw, &located(&[(26, 0)]));
        assert_eq!(reading.text, String::from_utf8_lossy(&raw));
        // binary and designators outside the table: one U+FFFD per byte
        for value in [899, 0, 2, 14, 19, 170, 999, 999_999] {
            let reading = read(&[0x00, 0xFF], &located(&[(value, 0)]));
            assert_eq!(reading.text, "\u{FFFD}\u{FFFD}", "ECI {value}");
            assert_eq!(reading.label, Label::Unknown, "ECI {value}");
        }
        // decodable segments keep their reading next to a marked one
        let reading = read(b"ab\xA1", &located(&[(26, 0), (999, 2)]));
        assert_eq!(reading.text, "ab\u{FFFD}");
        assert_eq!(reading.label, Label::Unknown);
    }

    #[test]
    fn a_designator_governing_no_byte_still_decides_the_label() {
        // ECI 999 immediately superseded: no byte is marked, but the content
        // lists a designator outside the table
        let reading = read(b"ok", &located(&[(999, 0), (26, 0)]));
        assert_eq!(reading.text, "ok");
        assert_eq!(reading.label, Label::Unknown);
        // same for binary
        let reading = read(b"ok", &located(&[(26, 0), (899, 2)]));
        assert_eq!(
            (reading.text.as_str(), reading.label),
            ("ok", Label::Unknown)
        );
        // and a zero-length designator of another family breaks the legacy
        // label: utf8 + latin1 listed
        let reading = read(b"ok", &located(&[(3, 0), (26, 0)]));
        assert_eq!(reading.label, Label::Eci);
    }

    #[test]
    fn signalled_only_content_is_marked_unknown() {
        let reading = read(&[0xA1, 0x42], &EciEvidence::Signalled);
        assert_eq!(reading.text, "\u{FFFD}\u{FFFD}");
        assert_eq!(reading.label, Label::Unknown);
        assert_eq!(read(&[], &EciEvidence::Signalled).text, "");
        // never published as latin1/shift_jis, even where the heuristic
        // would pick them for the same bytes
        for raw in [&[0xA1][..], &KONNICHIWA_SJIS, "é".as_bytes()] {
            let reading = read(raw, &EciEvidence::Signalled);
            assert_eq!(reading.text, replacement(raw.len()));
            assert_eq!(reading.published(), Charset::Utf8, "{raw:?}");
        }
    }

    #[test]
    fn iso_8859_9_and_11_are_exact_in_crate_tables() {
        // 8859-9: C1 controls stay controls; the A0-FF half equals the
        // windows-1254 superset (independent oracle)
        let reading = read(&[0xD0, 0x80], &located(&[(11, 0)]));
        assert_eq!(reading.text, "Ğ\u{80}");
        let upper: Vec<u8> = (0xA0..=0xFF).collect();
        let (oracle, _) = encoding_rs::WINDOWS_1254.decode_without_bom_handling(&upper);
        assert_eq!(read(&upper, &located(&[(11, 0)])).text, oracle);
        // 8859-11: Thai; DB-DE and FC-FF unassigned
        assert_eq!(read(&[0xA1, 0xDF], &located(&[(13, 0)])).text, "ก฿");
        let reading = read(&[0xDB], &located(&[(13, 0)]));
        assert_eq!(
            (reading.text.as_str(), reading.label),
            ("\u{FFFD}", Label::Unknown)
        );
        let assigned: Vec<u8> = (0xA0..=0xFB)
            .filter(|b| !(0xDB..=0xDE).contains(b))
            .collect();
        let (oracle, _) = encoding_rs::WINDOWS_874.decode_without_bom_handling(&assigned);
        assert_eq!(read(&assigned, &located(&[(13, 0)])).text, oracle);
        assert_eq!(read(&[0x80], &located(&[(13, 0)])).text, "\u{80}");
    }

    #[test]
    fn the_rest_of_the_decode_table() {
        // GB 2312 / GBK / GB 18030 through the gb18030 decoder
        for value in [29, 31, 32] {
            assert_eq!(read(&[0xB0, 0xA1], &located(&[(value, 0)])).text, "啊");
        }
        assert_eq!(read(&[0xA4, 0x40], &located(&[(28, 0)])).text, "一");
        assert_eq!(read(&[0xB0, 0xA1], &located(&[(30, 0)])).text, "가");
        assert_eq!(read(&[0xC1], &located(&[(21, 0)])).text, "Á");
        assert_eq!(read(&[0xC0], &located(&[(22, 0)])).text, "А");
        assert_eq!(read(&[0xC7], &located(&[(24, 0)])).text, "ا");
        assert_eq!(read(&[0x00, 0x41], &located(&[(25, 0)])).text, "A");
        assert_eq!(read(&[0x41, 0x00], &located(&[(33, 0)])).text, "A");
        // an odd UTF-16 byte is one marked subpart
        let reading = read(&[0x00, 0x41, 0x00], &located(&[(25, 0)]));
        assert_eq!(
            (reading.text.as_str(), reading.label),
            ("A\u{FFFD}", Label::Unknown)
        );
        // UTF-32 in-crate: a surrogate and a truncated unit are marked
        assert_eq!(read(&[0, 0, 0, 0x41], &located(&[(34, 0)])).text, "A");
        assert_eq!(read(&[0x41, 0, 0, 0], &located(&[(35, 0)])).text, "A");
        let reading = read(&[0, 0, 0xD8, 0, 0, 0], &located(&[(34, 0)]));
        assert_eq!(
            (reading.text.as_str(), reading.label),
            ("\u{FFFD}\u{FFFD}", Label::Unknown)
        );
        // ASCII: bytes from 0x80 have no reading
        let reading = read(&[0x41, 0xA1], &located(&[(27, 0)]));
        assert_eq!(
            (reading.text.as_str(), reading.label),
            ("A\u{FFFD}", Label::Unknown)
        );
        // the WHATWG decoders never sniff a byte-order mark
        assert_eq!(
            read(&[0xEF, 0xBB, 0xBF, 0x41], &located(&[(26, 0)])).text,
            "\u{FEFF}A"
        );
    }

    #[test]
    fn evidence_lattice_and_join() {
        let none = EciEvidence::NoEci;
        let eci4 = located(&[(4, 0)]);
        let eci7 = located(&[(7, 0)]);
        let signalled = EciEvidence::Signalled;
        let unobserved = EciEvidence::Unobserved;
        let cases = [
            (&unobserved, &eci4, true),
            (&unobserved, &none, true),
            (&unobserved, &signalled, true),
            (&none, &none, true),
            (&none, &signalled, false),
            (&none, &eci4, false),
            (&signalled, &signalled, true),
            (&signalled, &eci4, true),
            (&eci4, &eci4, true),
            (&eci4, &eci7, false),
        ];
        for (a, b, agree) in cases {
            assert_eq!(a.agrees_with(b), agree, "{a:?} vs {b:?}");
            assert_eq!(b.agrees_with(a), agree, "symmetric: {b:?} vs {a:?}");
        }
        let mut key = ContentKey {
            raw: vec![0xA1],
            eci: EciEvidence::Signalled,
            fnc1: Fnc1::None,
        };
        key.join(eci4.clone(), Fnc1::None);
        assert_eq!(key.eci, eci4, "located over signalled");
        key.join(EciEvidence::Signalled, Fnc1::None);
        assert_eq!(key.eci, eci4, "never back to signalled");
        let mut key = ContentKey {
            raw: vec![0xA1],
            eci: EciEvidence::Unobserved,
            fnc1: Fnc1::Unobserved,
        };
        key.join(EciEvidence::NoEci, Fnc1::First);
        assert_eq!((&key.eci, key.fnc1), (&EciEvidence::NoEci, Fnc1::First));
        // an observed position is kept
        key.join(EciEvidence::NoEci, Fnc1::Unobserved);
        assert_eq!(key.fnc1, Fnc1::First);
    }

    #[test]
    fn observed_fnc1_positions_agree_only_with_themselves() {
        use Fnc1 as F;
        for (a, b, agree) in [
            (F::None, F::None, true),
            (F::First, F::First, true),
            (F::Second, F::Second, true),
            (F::None, F::First, false),
            (F::None, F::Second, false),
            (F::First, F::Second, false),
            (F::Unobserved, F::None, true),
            (F::Unobserved, F::First, true),
            (F::Unobserved, F::Second, true),
            (F::Unobserved, F::Unobserved, true),
        ] {
            assert_eq!(a.agrees_with(b), agree, "{a:?} vs {b:?}");
            assert_eq!(b.agrees_with(a), agree, "symmetric: {b:?} vs {a:?}");
        }
    }

    #[test]
    fn content_key_admits_same_bytes_agreeing_accounts_and_fnc1_positions() {
        let hit = |raw: &[u8], eci: EciEvidence, fnc1: Fnc1| RawDetection {
            symbology: Symbology::QrCode,
            raw: raw.to_vec(),
            eci,
            masked_stream: None,
            corners: None,
            version: None,
            ec: None,
            mask: None,
            fnc1,
            parsed: false,
            engine: EngineKind::Rxing,
        };
        let key = ContentKey {
            raw: vec![0xA1],
            eci: located(&[(4, 0)]),
            fnc1: Fnc1::None,
        };
        assert!(key.admits(&hit(&[0xA1], EciEvidence::Signalled, Fnc1::None)));
        assert!(key.admits(&hit(&[0xA1], EciEvidence::Unobserved, Fnc1::None)));
        assert!(!key.admits(&hit(&[0xA1], EciEvidence::NoEci, Fnc1::None)));
        assert!(!key.admits(&hit(&[0xA2], EciEvidence::Signalled, Fnc1::None)));
        let gs1 = ContentKey {
            raw: b"10AB".to_vec(),
            eci: EciEvidence::NoEci,
            fnc1: Fnc1::First,
        };
        assert!(gs1.admits(&hit(b"10AB", EciEvidence::NoEci, Fnc1::First)));
        assert!(gs1.admits(&hit(b"10AB", EciEvidence::NoEci, Fnc1::Unobserved)));
        assert!(
            !gs1.admits(&hit(b"10AB", EciEvidence::NoEci, Fnc1::None)),
            "an observed plain symbol is another content"
        );
        assert!(
            !gs1.admits(&hit(b"10AB", EciEvidence::NoEci, Fnc1::Second)),
            "first and second position are different symbols"
        );
    }

    #[test]
    fn inconsistent_offsets_never_panic() {
        let reading = read(b"ab", &located(&[(26, 5)]));
        assert_eq!(reading.text, "ab", "clamped: every byte before it");
        let reading = read(b"ab", &located(&[(26, 1), (4, 0)]));
        assert_eq!(reading.text, "ab");
    }

    fn evidence_strategy() -> impl Strategy<Value = EciEvidence> {
        let mark = (
            prop_oneof![
                Just(3u32),
                Just(4),
                Just(20),
                Just(26),
                Just(899),
                Just(999),
                0u32..1000
            ],
            0usize..12,
        );
        prop_oneof![
            Just(EciEvidence::Unobserved),
            Just(EciEvidence::NoEci),
            Just(EciEvidence::Signalled),
            proptest::collection::vec(mark, 1..4).prop_map(|marks| EciEvidence::Located(
                marks
                    .into_iter()
                    .map(|(value, start)| EciMark { value, start })
                    .collect()
            )),
        ]
    }

    proptest! {
        /// Global invariants of the one interpretation, over arbitrary bytes
        /// and arbitrary (even inconsistent) evidence: never empty text over
        /// non-empty bytes, deterministic, and a legacy label always
        /// reproduces the text from the bytes exactly.
        #[test]
        fn readings_are_total_deterministic_and_label_consistent(
            raw in proptest::collection::vec(any::<u8>(), 0..12),
            evidence in evidence_strategy(),
        ) {
            let reading = read(&raw, &evidence);
            prop_assert!(raw.is_empty() || !reading.text.is_empty());
            prop_assert_eq!(&reading, &read(&raw, &evidence));
            if let Label::Legacy(label) = reading.label {
                let decoded = charset::decode_legacy(&raw, label);
                prop_assert_eq!(decoded.as_deref(), Some(reading.text.as_str()));
            }
        }
    }
}
