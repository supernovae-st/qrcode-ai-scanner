//! QR content observations and their ONE interpretation (ISO/IEC 18004
//! §7.3.5 · §7.4.2).
//!
//! An observation is payload bytes plus what its source could see of the
//! segment structure: whether an ECI designator was present and — when the
//! scanner parsed the RS-corrected codewords itself — which assignment
//! number governs which bytes. [`read`] turns that into text exactly once,
//! for every consumer (ladder grouping, the published content, stress-cell
//! matching):
//!
//! 1. A declared ECI governs its bytes until the next designator or the end
//!    of data. Bytes before the first designator are ISO/IEC 8859-1 (the
//!    default interpretation, §7.3.5) — Kanji/Hanzi pairs there carry their
//!    own set, since no ECI governs them yet.
//! 2. Under an ECI every mode only compacts bytes (§7.4.2.1): a Kanji pair
//!    after ECI 26 is two UTF-8 bytes, not Shift JIS.
//! 3. A payload without any designator keeps the charset heuristic
//!    ([`charset::resolve`]) and its label, unchanged.
//! 4. A declaration the scanner cannot honour — an assignment number with no
//!    registered character set, bytes invalid in the declared set, ECI 899
//!    binary, a designator an engine signalled but could not locate —
//!    keeps every byte and publishes the lossless byte rendering (each byte
//!    as the code point of the same value), never a guessed reading.

use std::borrow::Cow;
use std::ops::Range;

use super::charset;
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

/// A double-byte mode — its pairs carry their own character set where no
/// ECI governs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DoubleByteMode {
    /// Kanji mode (§7.4.6): Shift JIS (JIS X 0208) pairs.
    Kanji,
    /// Hanzi mode (GB/T 18284): GB 2312 pairs.
    Hanzi,
}

/// Payload bytes `start..end`, written by one double-byte segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DoubleByteSpan {
    /// The segment's mode.
    pub(crate) mode: DoubleByteMode,
    /// First payload byte of the segment.
    pub(crate) start: usize,
    /// One past its last payload byte.
    pub(crate) end: usize,
}

/// The parts of a segment stream the interpretation depends on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SegmentMap {
    /// ECI designators, in stream order.
    pub(crate) eci: Vec<EciMark>,
    /// Double-byte segments, in stream order.
    pub(crate) double_byte: Vec<DoubleByteSpan>,
}

/// What one observation could see of the ECI declarations of its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EciEvidence {
    /// Nothing observable: a symbology outside the QR family, or a stream
    /// re-derivation that failed or told another story than the engine's
    /// bytes (e.g. the unmirrored read of a mirrored symbol).
    Unobserved,
    /// The engine stated that no designator is present (AIM modifier
    /// `]Q0`/`]Q1`/`]Q3`/`]Q5`); the structure is otherwise unknown.
    Absent,
    /// The engine stated a designator is present (`]Q2`/`]Q4`/`]Q6`) but
    /// exposes neither its number nor its position. Its own text rides
    /// along — kept only when it is a faithful decode of the bytes.
    Unlocated {
        /// The engine's decoded text.
        engine_text: String,
    },
    /// The whole structure, parsed by the scanner from RS-corrected data
    /// codewords.
    Parsed(SegmentMap),
}

impl EciEvidence {
    /// Whether two observations of the same bytes can be one physical
    /// symbol: their ECI accounts must not contradict each other — an
    /// unmarked `A1` and an ECI-4 `A1` are two different contents.
    pub(crate) fn agrees_with(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Unobserved, _)
            | (_, Self::Unobserved)
            | (Self::Absent, Self::Absent)
            | (Self::Unlocated { .. }, Self::Unlocated { .. }) => true,
            (Self::Parsed(a), Self::Parsed(b)) => a == b,
            (Self::Parsed(map), Self::Unlocated { .. })
            | (Self::Unlocated { .. }, Self::Parsed(map)) => !map.eci.is_empty(),
            (Self::Parsed(map), Self::Absent) | (Self::Absent, Self::Parsed(map)) => {
                map.eci.is_empty()
            }
            (Self::Absent, Self::Unlocated { .. }) | (Self::Unlocated { .. }, Self::Absent) => {
                false
            }
        }
    }

    /// How much this account says: a parsed structure beats an engine's
    /// presence flag, which beats nothing.
    fn weight(&self) -> u8 {
        match self {
            Self::Unobserved => 0,
            Self::Absent | Self::Unlocated { .. } => 1,
            Self::Parsed(_) => 2,
        }
    }

    /// Keep the more informative of two agreeing accounts (the first one
    /// on a tie — deterministic in absorb order).
    pub(crate) fn absorb(&mut self, other: Self) {
        if other.weight() > self.weight() {
            *self = other;
        }
    }
}

/// How the scanner reached the text — the honest account behind the label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentStatus {
    /// No designator observed: the charset heuristic read the bytes.
    Heuristic(Charset),
    /// Every byte decoded under its declaration. `Some` names the legacy
    /// label whose decoder reproduces the text from the bytes exactly.
    Declared(Option<Charset>),
    /// A designator was signalled but not located; the engine's text is a
    /// strict decode of the bytes under a registered ECI character set, so
    /// it was kept. `Some`: as for `Declared`.
    EngineReading(Option<Charset>),
    /// A designator was signalled but not located, and the engine's text is
    /// not a faithful decode of the bytes.
    UnlocatedEci,
    /// ECI 000899: 8-bit binary data, no character interpretation.
    Binary,
    /// A declared assignment number with no registered character set the
    /// scanner can decode (CP437, unassigned numbers, 900 and above).
    UnsupportedEci,
    /// Declared bytes that are invalid in their declared character set.
    Undecodable,
}

/// The text of one observation and how it was reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reading {
    /// The text — a faithful reading, or the lossless byte rendering when
    /// `status` says no faithful reading exists.
    pub(crate) text: String,
    /// How `text` was reached.
    pub(crate) status: ContentStatus,
}

impl Reading {
    /// The label published under the three-label wire contract. A faithful
    /// reading publishes the legacy label that reproduces its text; a
    /// reading no legacy label can express (an ISO/IEC 8859-2 text, a
    /// byte rendering) keeps the heuristic's label for the bytes — `status`
    /// carries the honest account.
    pub(crate) fn label(&self, raw: &[u8]) -> Charset {
        match self.status {
            ContentStatus::Heuristic(label)
            | ContentStatus::Declared(Some(label))
            | ContentStatus::EngineReading(Some(label)) => label,
            ContentStatus::Declared(None)
            | ContentStatus::EngineReading(None)
            | ContentStatus::UnlocatedEci
            | ContentStatus::Binary
            | ContentStatus::UnsupportedEci
            | ContentStatus::Undecodable => charset::resolve(raw).1,
        }
    }
}

/// Interpret one observation — THE content rule of the crate (module doc).
pub(crate) fn read(raw: &[u8], eci: &EciEvidence) -> Reading {
    match eci {
        EciEvidence::Parsed(map) if !map.eci.is_empty() => read_declared(raw, map),
        EciEvidence::Unlocated { engine_text } => read_unlocated(raw, engine_text),
        EciEvidence::Unobserved | EciEvidence::Absent | EciEvidence::Parsed(_) => {
            let (text, label) = charset::resolve(raw);
            Reading {
                text,
                status: ContentStatus::Heuristic(label),
            }
        }
    }
}

/// A character set an ECI assignment number declares (AIM ECI register,
/// character-set block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EciSet {
    /// ISO/IEC 8859-1 — byte n is U+00nn, C1 controls included (never the
    /// Windows-1252 superset the heuristic's `latin1` label decodes).
    Latin1,
    /// An ISO/IEC 8859 part `encoding_rs` only models through its Windows
    /// superset: 0x80-0x9F are C1 controls in the ISO part and decode as
    /// such; every other byte goes through the superset table.
    IsoPart(&'static encoding_rs::Encoding),
    /// Decoded by `encoding_rs` exactly as registered.
    Whatwg(&'static encoding_rs::Encoding),
    /// US-ASCII (ECI 27): seven-bit only.
    Ascii,
    /// ISO/IEC 646 invariant (ECI 170): seven-bit minus the twelve
    /// positions national variants redefine.
    Iso646Invariant,
    /// UTF-32 (ECI 34 big endian, ECI 35 little endian).
    Utf32 {
        /// Byte order of each code unit.
        big_endian: bool,
    },
    /// ECI 000899: binary — no character interpretation exists.
    Binary,
}

/// The character set an assignment number declares; `None` when the
/// scanner has none for it (CP437 = ECI 0/2, unassigned 14/19, 36-169,
/// 171-898, and everything from 900 on).
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
        11 => EciSet::IsoPart(e::WINDOWS_1254), // ISO/IEC 8859-9
        12 => EciSet::Whatwg(e::ISO_8859_10),
        13 => EciSet::IsoPart(e::WINDOWS_874), // ISO/IEC 8859-11
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
        // GB 2312: the GBK decoder reads every GB 2312 sequence unchanged
        29 | 31 => EciSet::Whatwg(e::GBK),
        30 => EciSet::Whatwg(e::EUC_KR),
        32 => EciSet::Whatwg(e::GB18030),
        33 => EciSet::Whatwg(e::UTF_16LE),
        34 => EciSet::Utf32 { big_endian: true },
        35 => EciSet::Utf32 { big_endian: false },
        170 => EciSet::Iso646Invariant,
        899 => EciSet::Binary,
        _ => return None,
    })
}

/// The twelve ISO/IEC 646 positions national variants redefine (number
/// sign, dollar, at, brackets, backslash, circumflex, grave accent, braces,
/// vertical line, tilde) — outside the invariant set.
const ISO_646_NATIONAL: &[u8; 12] = b"#$@[\\]^`{|}~";

fn is_c1(byte: u8) -> bool {
    (0x80..=0x9F).contains(&byte)
}

/// Strictly decode `bytes` under `set`: every byte must belong to the set
/// — no replacement characters, no BOM sniffing. Binary never decodes.
fn decode_strict(bytes: &[u8], set: EciSet) -> Option<String> {
    match set {
        EciSet::Latin1 => Some(charset::iso_8859_1(bytes)),
        EciSet::Whatwg(encoding) => encoding
            .decode_without_bom_handling_and_without_replacement(bytes)
            .map(Cow::into_owned),
        EciSet::IsoPart(superset) => {
            let mut text = String::with_capacity(bytes.len());
            for run in bytes.chunk_by(|a, b| is_c1(*a) == is_c1(*b)) {
                if run.first().is_some_and(|&b| is_c1(b)) {
                    text.push_str(&charset::iso_8859_1(run));
                } else {
                    text.push_str(
                        &superset.decode_without_bom_handling_and_without_replacement(run)?,
                    );
                }
            }
            Some(text)
        }
        EciSet::Ascii => bytes.is_ascii().then(|| charset::iso_8859_1(bytes)),
        EciSet::Iso646Invariant => bytes
            .iter()
            .all(|b| b.is_ascii() && !ISO_646_NATIONAL.contains(b))
            .then(|| charset::iso_8859_1(bytes)),
        EciSet::Utf32 { big_endian } => {
            let units = bytes.chunks_exact(4);
            if !units.remainder().is_empty() {
                return None;
            }
            units
                .map(|unit| {
                    let unit = <[u8; 4]>::try_from(unit).ok()?;
                    char::from_u32(if big_endian {
                        u32::from_be_bytes(unit)
                    } else {
                        u32::from_le_bytes(unit)
                    })
                })
                .collect()
        }
        EciSet::Binary => None,
    }
}

/// The legacy label whose decoder agrees with `set` on non-ASCII bytes.
fn legacy_family(set: EciSet) -> Option<Charset> {
    match set {
        EciSet::Latin1 => Some(Charset::Latin1),
        EciSet::Whatwg(encoding) if encoding == encoding_rs::WINDOWS_1252 => Some(Charset::Latin1),
        EciSet::Whatwg(encoding) if encoding == encoding_rs::UTF_8 => Some(Charset::Utf8),
        EciSet::Whatwg(encoding) if encoding == encoding_rs::SHIFT_JIS => Some(Charset::ShiftJis),
        _ => None,
    }
}

/// `family` as a published label, only when its legacy decoder reproduces
/// `text` from the whole payload (the label/text pair stays consistent).
fn faithful_label(raw: &[u8], text: &str, family: Option<Charset>) -> Option<Charset> {
    family.filter(|&label| charset::decode_legacy(raw, label).as_deref() == Some(text))
}

/// Each payload range with the character set that governs it (`None`: an
/// assignment number the scanner has no set for).
fn spans(len: usize, map: &SegmentMap) -> Vec<(Range<usize>, Option<EciSet>)> {
    let mut out = Vec::new();
    let first = map.eci.first().map_or(len, |mark| mark.start.min(len));
    // before the first designator: ISO/IEC 8859-1, except double-byte
    // pairs — no ECI governs them yet, so their mode's own set does
    let mut cursor = 0;
    for span in &map.double_byte {
        let (start, end) = (span.start.max(cursor), span.end.min(first));
        if start >= end {
            continue;
        }
        if start > cursor {
            out.push((cursor..start, Some(EciSet::Latin1)));
        }
        let native = match span.mode {
            DoubleByteMode::Kanji => encoding_rs::SHIFT_JIS,
            DoubleByteMode::Hanzi => encoding_rs::GBK,
        };
        out.push((start..end, Some(EciSet::Whatwg(native))));
        cursor = end;
    }
    if first > cursor {
        out.push((cursor..first, Some(EciSet::Latin1)));
    }
    for (i, mark) in map.eci.iter().enumerate() {
        let end = map.eci.get(i + 1).map_or(len, |next| next.start);
        out.push((mark.start..end, eci_set(mark.value)));
    }
    out
}

fn read_declared(raw: &[u8], map: &SegmentMap) -> Reading {
    let mut text = String::with_capacity(raw.len());
    // the label family shared by every non-ASCII span: None = none seen
    // yet, Some(None) = no single legacy label covers them
    let mut family: Option<Option<Charset>> = None;
    let (mut unsupported, mut undecodable, mut binary) = (false, false, false);
    for (range, set) in spans(raw.len(), map) {
        let Some(bytes) = raw.get(range) else {
            undecodable = true;
            continue;
        };
        if bytes.is_empty() {
            continue; // a declaration that governs no byte claims nothing
        }
        match set {
            None => unsupported = true,
            Some(EciSet::Binary) => binary = true,
            Some(set) => match decode_strict(bytes, set) {
                Some(part) => {
                    text.push_str(&part);
                    if !bytes.is_ascii() {
                        let own = legacy_family(set);
                        family = Some(match family {
                            None => own,
                            Some(shared) if shared == own => shared,
                            Some(_) => None,
                        });
                    }
                }
                None => undecodable = true,
            },
        }
    }
    let status = if unsupported {
        ContentStatus::UnsupportedEci
    } else if undecodable {
        ContentStatus::Undecodable
    } else if binary {
        ContentStatus::Binary
    } else {
        // all-ASCII payloads read the same under every label: utf8, as the
        // heuristic says for them
        let label = faithful_label(raw, &text, family.unwrap_or(Some(Charset::Utf8)));
        return Reading {
            text,
            status: ContentStatus::Declared(label),
        };
    };
    Reading {
        text: charset::iso_8859_1(raw),
        status,
    }
}

fn read_unlocated(raw: &[u8], engine_text: &str) -> Reading {
    // The engine decoded spans we cannot see. Its text is kept only when a
    // registered character set reproduces it from the bytes exactly — then
    // it IS a faithful reading of them, whatever the declared number was.
    // Assignment order makes the pick deterministic.
    let matching = (0..=899)
        .filter_map(eci_set)
        .find(|&set| decode_strict(raw, set).as_deref() == Some(engine_text));
    match matching {
        Some(set) => {
            let family = if raw.is_ascii() {
                Some(Charset::Utf8)
            } else {
                legacy_family(set)
            };
            Reading {
                text: engine_text.to_owned(),
                status: ContentStatus::EngineReading(faithful_label(raw, engine_text, family)),
            }
        }
        None => Reading {
            text: charset::iso_8859_1(raw),
            status: ContentStatus::UnlocatedEci,
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use proptest::prelude::*;

    use super::*;

    fn declared(marks: &[(u32, usize)]) -> EciEvidence {
        EciEvidence::Parsed(SegmentMap {
            eci: marks
                .iter()
                .map(|&(value, start)| EciMark { value, start })
                .collect(),
            double_byte: Vec::new(),
        })
    }

    fn rendering(raw: &[u8]) -> String {
        raw.iter().map(|&b| char::from(b)).collect()
    }

    const KONNICHIWA_SJIS: [u8; 10] = [0x82, 0xB1, 0x82, 0xF1, 0x82, 0xC9, 0x82, 0xBF, 0x82, 0xCD];

    #[test]
    fn no_designator_keeps_the_heuristic_and_its_label() {
        // Absent, Unobserved and a parsed stream without designators all
        // read exactly as charset::resolve — labels unchanged.
        for evidence in [
            EciEvidence::Absent,
            EciEvidence::Unobserved,
            EciEvidence::Parsed(SegmentMap::default()),
        ] {
            for raw in [
                &[0xA1][..],
                &[0x35, 0x80],
                "héllo → 🦋".as_bytes(),
                &KONNICHIWA_SJIS,
            ] {
                let (text, label) = charset::resolve(raw);
                let reading = read(raw, &evidence);
                assert_eq!(reading.text, text, "{evidence:?} {raw:?}");
                assert_eq!(reading.status, ContentStatus::Heuristic(label));
                assert_eq!(reading.label(raw), label);
            }
        }
    }

    #[test]
    fn a_declared_eci_wins_over_the_heuristic() {
        // ECI 4 (ISO/IEC 8859-2): A1 is 'Ą' — no legacy label reproduces it
        let reading = read(&[0xA1], &declared(&[(4, 0)]));
        assert_eq!(reading.text, "\u{104}");
        assert_eq!(reading.status, ContentStatus::Declared(None));
        // ECI 7 (ISO/IEC 8859-5)
        let privet = [0xBF, 0xE0, 0xD8, 0xD2, 0xD5, 0xE2];
        assert_eq!(read(&privet, &declared(&[(7, 0)])).text, "Привет");
    }

    #[test]
    fn eci_1_and_3_are_strict_iso_8859_1() {
        // 0x80 is the C1 control U+0080 under ISO/IEC 8859-1 — the
        // Windows-1252 '€' would be a different character, so the legacy
        // label (which decodes Windows-1252) cannot name this reading
        for value in [1, 3] {
            let reading = read(&[0x35, 0x80], &declared(&[(value, 0)]));
            assert_eq!(reading.text, "5\u{80}");
            assert_eq!(reading.status, ContentStatus::Declared(None));
        }
        // outside 0x80-0x9F the two agree: the legacy label is faithful
        let reading = read(&[0xA1], &declared(&[(3, 0)]));
        assert_eq!(reading.text, "¡");
        assert_eq!(
            reading.status,
            ContentStatus::Declared(Some(Charset::Latin1))
        );
    }

    #[test]
    fn faithful_declarations_keep_their_legacy_label() {
        let reading = read("\u{104}".as_bytes(), &declared(&[(26, 0)]));
        assert_eq!(reading.status, ContentStatus::Declared(Some(Charset::Utf8)));
        let reading = read(&KONNICHIWA_SJIS, &declared(&[(20, 0)]));
        assert_eq!(reading.text, "こんにちは");
        assert_eq!(
            reading.status,
            ContentStatus::Declared(Some(Charset::ShiftJis))
        );
        // ASCII under any declaration reads the same everywhere: utf8
        let reading = read(b"abc", &declared(&[(4, 0)]));
        assert_eq!(reading.status, ContentStatus::Declared(Some(Charset::Utf8)));
    }

    #[test]
    fn each_declaration_governs_until_the_next() {
        let raw = [0xA1, 0xE2, 0x82, 0xAC];
        let reading = read(&raw, &declared(&[(4, 0), (26, 1)]));
        assert_eq!(reading.text, "\u{104}€");
        assert_eq!(reading.status, ContentStatus::Declared(None));
    }

    #[test]
    fn bytes_before_the_first_designator_are_iso_8859_1() {
        let reading = read(b"12345\xA1", &declared(&[(4, 5)]));
        assert_eq!(reading.text, "12345\u{104}");
        // C1 bytes there are controls, not Windows-1252 punctuation
        let reading = read(&[0x80, 0xA1], &declared(&[(4, 1)]));
        assert_eq!(reading.text, "\u{80}\u{104}");
    }

    #[test]
    fn kanji_before_any_designator_is_shift_jis_and_after_one_is_bytes() {
        // Kanji pairs ahead of every designator: their own set (Shift JIS)
        let mut raw = KONNICHIWA_SJIS.to_vec();
        raw.extend_from_slice("é".as_bytes());
        let evidence = EciEvidence::Parsed(SegmentMap {
            eci: vec![EciMark {
                value: 26,
                start: 10,
            }],
            double_byte: vec![DoubleByteSpan {
                mode: DoubleByteMode::Kanji,
                start: 0,
                end: 10,
            }],
        });
        let reading = read(&raw, &evidence);
        assert_eq!(reading.text, "こんにちはé");
        // two label families (Shift JIS + UTF-8): no single legacy label
        assert_eq!(reading.status, ContentStatus::Declared(None));

        // the same pairs AFTER ECI 26 are UTF-8 bytes (§7.4.2.1) — invalid
        let mut raw = "é".as_bytes().to_vec();
        raw.extend_from_slice(&KONNICHIWA_SJIS);
        let evidence = EciEvidence::Parsed(SegmentMap {
            eci: vec![EciMark {
                value: 26,
                start: 0,
            }],
            double_byte: vec![DoubleByteSpan {
                mode: DoubleByteMode::Kanji,
                start: 2,
                end: 12,
            }],
        });
        let reading = read(&raw, &evidence);
        assert_eq!(reading.status, ContentStatus::Undecodable);
        assert_eq!(reading.text, rendering(&raw));
    }

    #[test]
    fn hanzi_before_any_designator_is_gb2312() {
        // 0xB0A1 = '啊' in GB 2312, then ECI 26 'a'
        let evidence = EciEvidence::Parsed(SegmentMap {
            eci: vec![EciMark {
                value: 26,
                start: 2,
            }],
            double_byte: vec![DoubleByteSpan {
                mode: DoubleByteMode::Hanzi,
                start: 0,
                end: 2,
            }],
        });
        assert_eq!(read(&[0xB0, 0xA1, b'a'], &evidence).text, "啊a");
    }

    #[test]
    fn declarations_the_scanner_cannot_honour_keep_every_byte() {
        // unassigned / CP437 / past the character-set block: unsupported
        for value in [0, 2, 14, 999, 999_999] {
            let reading = read(&[0xA1], &declared(&[(value, 0)]));
            assert_eq!(reading.status, ContentStatus::UnsupportedEci, "ECI {value}");
            assert_eq!(reading.text, "¡", "lossless rendering, ECI {value}");
        }
        // bytes invalid in the declared set
        let reading = read(&[0xA1], &declared(&[(26, 0)]));
        assert_eq!(reading.status, ContentStatus::Undecodable);
        assert_eq!(reading.text, "¡");
        // binary
        let raw = [0x00, 0xFF, 0x80, 0x7F];
        let reading = read(&raw, &declared(&[(899, 0)]));
        assert_eq!(reading.status, ContentStatus::Binary);
        assert_eq!(reading.text, rendering(&raw));
        // unsupported wins over a decodable neighbour: the whole payload
        // is rendered, never half a reading
        let reading = read(b"ab\xA1", &declared(&[(26, 0), (999, 2)]));
        assert_eq!(reading.status, ContentStatus::UnsupportedEci);
        assert_eq!(reading.text, "ab¡");
    }

    #[test]
    fn a_declaration_governing_no_byte_claims_nothing() {
        // ECI 999 immediately superseded by ECI 26: no byte is unsupported
        let reading = read(b"ok", &declared(&[(999, 0), (26, 0)]));
        assert_eq!(reading.status, ContentStatus::Declared(Some(Charset::Utf8)));
        assert_eq!(reading.text, "ok");
    }

    #[test]
    fn iso_parts_modelled_by_a_windows_superset_keep_c1_controls() {
        // ISO/IEC 8859-9 (ECI 11): 0xD0 is 'Ğ', 0x80 a C1 control — the
        // windows-1254 superset would read '€'
        let reading = read(&[0xD0, 0x80], &declared(&[(11, 0)]));
        assert_eq!(reading.text, "Ğ\u{80}");
        // ISO/IEC 8859-11 (ECI 13): 0xA1 is 'ก'; 0xDB is unassigned
        assert_eq!(read(&[0xA1], &declared(&[(13, 0)])).text, "ก");
        assert_eq!(
            read(&[0xDB], &declared(&[(13, 0)])).status,
            ContentStatus::Undecodable
        );
    }

    #[test]
    fn multibyte_unit_sets_are_strict() {
        assert_eq!(read(&[0x00, 0x41], &declared(&[(25, 0)])).text, "A");
        assert_eq!(read(&[0x41, 0x00], &declared(&[(33, 0)])).text, "A");
        assert_eq!(
            read(&[0x00], &declared(&[(25, 0)])).status,
            ContentStatus::Undecodable
        );
        assert_eq!(read(&[0, 0, 0, 0x41], &declared(&[(34, 0)])).text, "A");
        assert_eq!(read(&[0x41, 0, 0, 0], &declared(&[(35, 0)])).text, "A");
        // a surrogate code point is no character
        assert_eq!(
            read(&[0, 0, 0xD8, 0], &declared(&[(34, 0)])).status,
            ContentStatus::Undecodable
        );
        // seven-bit sets
        assert_eq!(read(b"A", &declared(&[(27, 0)])).text, "A");
        assert_eq!(
            read(&[0xA1], &declared(&[(27, 0)])).status,
            ContentStatus::Undecodable
        );
        assert_eq!(
            read(b"#", &declared(&[(170, 0)])).status,
            ContentStatus::Undecodable
        );
    }

    #[test]
    fn engine_text_for_unlocated_designators_must_be_a_faithful_decode() {
        let unlocated = |text: &str| EciEvidence::Unlocated {
            engine_text: text.to_owned(),
        };
        // ISO/IEC 8859-2 reproduces 'Ą' from A1: kept, no legacy label
        let reading = read(&[0xA1], &unlocated("\u{104}"));
        assert_eq!(reading.text, "\u{104}");
        assert_eq!(reading.status, ContentStatus::EngineReading(None));
        // an engine that dropped an undecodable span: rendering, explicit
        let reading = read(&[0xA1], &unlocated(""));
        assert_eq!(reading.status, ContentStatus::UnlocatedEci);
        assert_eq!(reading.text, "¡");
        // two declarations no single set reproduces: explicit, not trusted
        let reading = read(&[0xA1, 0xE2, 0x82, 0xAC], &unlocated("\u{104}€"));
        assert_eq!(reading.status, ContentStatus::UnlocatedEci);
        // ASCII: faithful under every label — utf8, as the heuristic says
        let reading = read(b"abc", &unlocated("abc"));
        assert_eq!(
            reading.status,
            ContentStatus::EngineReading(Some(Charset::Utf8))
        );
        // ECI 3 C1 text: ISO/IEC 8859-1 matches first — never a shift_jis
        // claim (whose decoder happens to map 0x80 to U+0080 too)
        let reading = read(&[0x35, 0x80], &unlocated("5\u{80}"));
        assert_eq!(reading.status, ContentStatus::EngineReading(None));
        // UTF-8 text under ECI 26
        let reading = read("\u{104}".as_bytes(), &unlocated("\u{104}"));
        assert_eq!(
            reading.status,
            ContentStatus::EngineReading(Some(Charset::Utf8))
        );
    }

    #[test]
    fn evidence_agreement_matrix() {
        let none = EciEvidence::Parsed(SegmentMap::default());
        let eci4 = declared(&[(4, 0)]);
        let eci7 = declared(&[(7, 0)]);
        let unlocated = EciEvidence::Unlocated {
            engine_text: String::new(),
        };
        let cases = [
            (&EciEvidence::Unobserved, &eci4, true),
            (&EciEvidence::Unobserved, &EciEvidence::Absent, true),
            (&EciEvidence::Absent, &none, true),
            (&EciEvidence::Absent, &eci4, false),
            (&unlocated, &eci4, true),
            (&unlocated, &none, false),
            (&unlocated, &EciEvidence::Absent, false),
            (&eci4, &eci4, true),
            (&eci4, &eci7, false),
            (&none, &eci4, false),
        ];
        for (a, b, agree) in cases {
            assert_eq!(a.agrees_with(b), agree, "{a:?} vs {b:?}");
            assert_eq!(b.agrees_with(a), agree, "symmetric: {b:?} vs {a:?}");
        }
        // the parsed structure is kept over a presence flag, never the
        // reverse; ties keep the first account
        let mut evidence = unlocated.clone();
        evidence.absorb(eci4.clone());
        assert_eq!(evidence, eci4);
        evidence.absorb(unlocated.clone());
        assert_eq!(evidence, eci4);
        let mut evidence = EciEvidence::Unobserved;
        evidence.absorb(EciEvidence::Absent);
        assert_eq!(evidence, EciEvidence::Absent);
    }

    #[test]
    fn inconsistent_offsets_never_panic_and_never_guess() {
        // offsets past the payload or out of order: undecodable rendering
        let reading = read(b"ab", &declared(&[(26, 5)]));
        assert_eq!(reading.text, "ab");
        assert_eq!(reading.status, ContentStatus::Undecodable);
        let reading = read(b"ab", &declared(&[(26, 1), (4, 0)]));
        assert_eq!(reading.status, ContentStatus::Undecodable);
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
        let span = (any::<bool>(), 0usize..12, 0usize..12);
        prop_oneof![
            Just(EciEvidence::Unobserved),
            Just(EciEvidence::Absent),
            ".{0,6}".prop_map(|engine_text| EciEvidence::Unlocated { engine_text }),
            (
                proptest::collection::vec(mark, 0..4),
                proptest::collection::vec(span, 0..3)
            )
                .prop_map(|(marks, spans)| EciEvidence::Parsed(SegmentMap {
                    eci: marks
                        .into_iter()
                        .map(|(value, start)| EciMark { value, start })
                        .collect(),
                    double_byte: spans
                        .into_iter()
                        .map(|(kanji, start, end)| DoubleByteSpan {
                            mode: if kanji {
                                DoubleByteMode::Kanji
                            } else {
                                DoubleByteMode::Hanzi
                            },
                            start,
                            end,
                        })
                        .collect(),
                })),
        ]
    }

    proptest! {
        /// Global invariants of the one interpretation, over arbitrary
        /// bytes and arbitrary (even inconsistent) evidence: never empty
        /// text over non-empty bytes, deterministic, and every published
        /// legacy label reproduces the text from the bytes exactly.
        #[test]
        fn readings_are_total_deterministic_and_label_consistent(
            raw in proptest::collection::vec(any::<u8>(), 0..12),
            evidence in evidence_strategy(),
        ) {
            let reading = read(&raw, &evidence);
            prop_assert!(raw.is_empty() || !reading.text.is_empty());
            prop_assert_eq!(&reading, &read(&raw, &evidence));
            let legacy = match reading.status {
                ContentStatus::Heuristic(label)
                | ContentStatus::Declared(Some(label))
                | ContentStatus::EngineReading(Some(label)) => Some(label),
                _ => None,
            };
            if let Some(label) = legacy {
                let decoded = charset::decode_legacy(&raw, label);
                prop_assert_eq!(decoded.as_deref(), Some(reading.text.as_str()));
            }
        }
    }
}
