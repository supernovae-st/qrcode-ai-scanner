//! QR content observations and their ONE interpretation (spec/01 § Content;
//! ISO/IEC 18004 §7.3.5 · §7.4.2 · §7.4.6 · §7.4.8; GB/T 18284).
//!
//! An observation is payload bytes plus what its source could see of the ECI
//! declarations, of the FNC1 position and of the Kanji/Hanzi segments. Two
//! reads are one content when their bytes are identical and their accounts
//! agree ([`ContentKey::admits`]); [`read`] then turns the content into text
//! exactly once, with the `charset` that says how:
//!
//! 1. No designator (`eci` absent) — the unmarked reading: a QR read whose
//!    declarations were not observable keeps ASCII and marks every other
//!    byte (1u); observed Kanji/Hanzi segments choose `Shift_JIS` or gb18030
//!    when every byte reads (1b, 1c), else what cannot is marked (1d); the
//!    decoding engine's own mode-aware reading may only choose between the
//!    scanner's complete decodings (1e); otherwise the legacy heuristic
//!    (1a, [`charset::resolve`]).
//! 2. Located designators govern their bytes until the next designator or
//!    the end of data; bytes before the first are ISO/IEC 8859-1. A decoder
//!    label is published only when its family holds every interpretation
//!    that governs a byte and its decoder reproduces the text; else `eci`.
//! 3. A designator signalled, never located: every byte marked, `unknown`.
//!
//! What cannot be decoded is marked, never transcribed nor guessed: WHATWG
//! replacement inside a supported set, one U+FFFD per byte under ECI 899, a
//! designator outside the decode table, or an unlocated declaration.

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
    /// a parsed stream, an rqrr decode (rqrr decodes neither FNC1 mode), or
    /// a symbology identifier that states none (`]d1`, `]C0`…).
    None,
    /// FNC1 in first position — GS1 data (`]Q3`/`]Q4`, `]d2`/`]d5`, `]C1`).
    First,
    /// FNC1 in second position — an AIM application, never GS1
    /// (`]Q5`/`]Q6`, `]d3`/`]d6`, `]C2`); its application indicator leads
    /// the payload bytes.
    Second,
    /// The position is unknown: an identifier that states nothing of it
    /// (`]e0`, `]z0`…), or none that parses.
    Unobserved,
}

impl Fnc1 {
    /// Observed positions agree only with themselves — a plain symbol, a
    /// GS1 one and an AIM one are three contents even over equal bytes;
    /// an unknown position agrees with any.
    pub(crate) fn agrees_with(self, other: Self) -> bool {
        self == other || self == Self::Unobserved || other == Self::Unobserved
    }
}

/// What one read could see of the ECI declarations of its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EciEvidence {
    /// Nothing observable: a symbology outside the QR family, a QR read whose
    /// identifier does not parse, an rqrr read whose grid decided nothing.
    Unobserved,
    /// No designator: stated by the engine (AIM modifier `]Q0`/`]Q1`/
    /// `]Q3`/`]Q5`, Micro QR) or observed in the parsed stream.
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
    /// ECI-4 `A1` stay two). A read without located evidence agrees with
    /// several contents of equal bytes: reconciliation lets its corner
    /// points decide, and joins nothing on ambiguity.
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
}

/// What one content IS: the reconciliation identity, and what score stress
/// cells and alpha probes compare an engine hit against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContentKey {
    /// Original payload bytes, designators excluded.
    pub(crate) raw: Vec<u8>,
    /// The ECI account.
    pub(crate) eci: EciEvidence,
    /// The FNC1 position.
    pub(crate) fnc1: Fnc1,
}

// The score cells and the alpha envelope match their reads by content
// identity through `admits` (spec/07 property 7).
impl ContentKey {
    /// The content one engine hit reports.
    #[cfg(test)]
    pub(crate) fn of(hit: &RawDetection) -> Self {
        Self {
            raw: hit.raw.clone(),
            eci: hit.eci.clone(),
            fnc1: hit.fnc1,
        }
    }

    /// Whether an engine hit reads this same content: identical bytes,
    /// agreeing ECI accounts, no FNC1 position conflict. Symbology is the
    /// caller's filter.
    pub(crate) fn admits(&self, hit: &RawDetection) -> bool {
        self.raw == hit.raw && self.eci.agrees_with(&hit.eci) && self.fnc1.agrees_with(hit.fnc1)
    }

    /// [`ContentKey::admits`] between two contents: whether they can be one.
    pub(crate) fn agrees(&self, other: &ContentKey) -> bool {
        self.raw == other.raw
            && self.eci.agrees_with(&other.eci)
            && self.fnc1.agrees_with(other.fnc1)
    }
}

/// The mode of a double-byte segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DoubleByteMode {
    /// Kanji (ISO/IEC 18004 §7.4.6): `Shift_JIS` pairs.
    Kanji,
    /// Hanzi (GB/T 18284, subset 1): GB 2312 pairs.
    Hanzi,
}

/// A run of payload bytes a parse read in Kanji or Hanzi mode: payload
/// offsets, `end` exclusive, an even length; adjacent segments of one mode
/// are one span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DoubleByteSpan {
    /// Kanji or Hanzi.
    pub(crate) mode: DoubleByteMode,
    /// First payload byte of the span.
    pub(crate) start: usize,
    /// One past its last byte.
    pub(crate) end: usize,
}

/// What the reads of one content observed of its double-byte segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Segments {
    /// No read parsed the data: the segments are unknown.
    NotObserved,
    /// The spans a parse observed (empty: none — all byte, numeric or
    /// alphanumeric data).
    Observed(Vec<DoubleByteSpan>),
    /// Parsed reads of one detection observed different spans.
    Conflicting,
}

/// The decoding engine's own mode-aware reading of a port read, when it
/// equals a complete double-byte decoding of all the bytes (rule 1e). It
/// only chooses between the scanner's decodings; its text is never kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(feature = "engine-rxing"),
    allow(dead_code, reason = "only rxing's port reads carry an engine reading")
)]
pub(crate) enum EngineReading {
    /// Nothing to choose from (or several engine readings disagreed).
    None,
    /// The engine's text is the `Shift_JIS` decoding of all the bytes.
    ShiftJis,
    /// The engine's text is the gb18030 decoding of all the bytes.
    Gb18030,
}

impl EngineReading {
    /// What an engine's text says of `raw`: `Shift_JIS` when the bytes are not
    /// UTF-8 and the text is exactly their error-free `Shift_JIS` decoding with
    /// at least one character from a byte pair; else gb18030 likewise; else
    /// nothing.
    #[cfg_attr(
        not(feature = "engine-rxing"),
        allow(dead_code, reason = "only rxing's port reads carry an engine reading")
    )]
    pub(crate) fn of(raw: &[u8], text: &str) -> Self {
        if std::str::from_utf8(raw).is_ok() {
            return Self::None;
        }
        let matches = |encoding: &'static encoding_rs::Encoding| {
            encoding
                .decode_without_bom_handling_and_without_replacement(raw)
                .is_some_and(|decoded| decoded == text)
                && has_pair(raw, encoding)
        };
        if matches(encoding_rs::SHIFT_JIS) {
            Self::ShiftJis
        } else if matches(encoding_rs::GB18030) {
            Self::Gb18030
        } else {
            Self::None
        }
    }
}

/// Whether well-formed `raw` holds a character encoded as a byte pair under
/// `encoding` (`Shift_JIS` or gb18030 — a gb18030 four-byte sequence is no
/// pair).
fn has_pair(raw: &[u8], encoding: &'static encoding_rs::Encoding) -> bool {
    let mut i = 0;
    while let Some(&lead) = raw.get(i) {
        let single = if encoding == encoding_rs::SHIFT_JIS {
            lead <= 0x80 || (0xA1..=0xDF).contains(&lead)
        } else {
            lead <= 0x80
        };
        if single {
            i += 1;
        } else if encoding == encoding_rs::GB18030
            && raw.get(i + 1).is_some_and(|b| (0x30..=0x39).contains(b))
        {
            i += 4;
        } else {
            return true;
        }
    }
    false
}

/// The text of one content and how it relates to the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reading {
    /// The text; U+FFFD marks what has no reading.
    pub(crate) text: String,
    /// The wire label (spec/01 § `text` and `charset`).
    pub(crate) charset: Charset,
}

/// Interpret one content — THE content rule of the crate (module doc).
/// `qr_family`: the symbology is QR, Micro QR or rMQR (rule 1u).
pub(crate) fn read(
    raw: &[u8],
    eci: &EciEvidence,
    segments: &Segments,
    engine: EngineReading,
    qr_family: bool,
) -> Reading {
    if raw.is_empty() {
        return Reading {
            text: String::new(),
            charset: Charset::Utf8,
        };
    }
    match eci {
        EciEvidence::Located(marks) => read_located(raw, marks),
        EciEvidence::Signalled => read_signalled(raw),
        EciEvidence::Unobserved if qr_family && !raw.is_ascii() => mark_unread(raw, &[]),
        EciEvidence::NoEci | EciEvidence::Unobserved => read_unmarked(raw, segments, engine),
    }
}

/// Rule 1 with the declarations observed absent (or not observable outside
/// the QR family).
fn read_unmarked(raw: &[u8], segments: &Segments, engine: EngineReading) -> Reading {
    match segments {
        // 1d without pairs: the reads of one detection disagree
        Segments::Conflicting => mark_unread(raw, &[]),
        Segments::Observed(spans) => read_segments(raw, spans),
        Segments::NotObserved => {
            if let Ok(text) = std::str::from_utf8(raw) {
                return Reading {
                    text: text.to_owned(),
                    charset: Charset::Utf8,
                };
            }
            read_engine_reading(raw, engine).unwrap_or_else(|| legacy(raw))
        }
    }
}

/// Rules 1a-1d over the segments a parse observed.
fn read_segments(raw: &[u8], spans: &[DoubleByteSpan]) -> Reading {
    let kanji = spans.iter().any(|s| s.mode == DoubleByteMode::Kanji);
    let hanzi = spans.iter().any(|s| s.mode == DoubleByteMode::Hanzi);
    match (kanji, hanzi) {
        (false, false) => legacy(raw),
        (true, false) => {
            // some encoders put UTF-8 bytes in Kanji mode
            if let Ok(text) = std::str::from_utf8(raw) {
                return Reading {
                    text: text.to_owned(),
                    charset: Charset::Utf8,
                };
            }
            double_byte(raw, spans, encoding_rs::SHIFT_JIS, Charset::ShiftJis)
                .unwrap_or_else(|| mark_unread(raw, spans))
        }
        (false, true) => double_byte(raw, spans, encoding_rs::GB18030, Charset::Gb18030)
            .unwrap_or_else(|| mark_unread(raw, spans)),
        (true, true) => mark_unread(raw, spans),
    }
}

/// Rules 1b/1c: the double-byte decoding of all of `raw`, when the decoder
/// reads every segment as well-formed pairs and every maximal run outside
/// the segments on its own, and no such run with a byte from 0x80 is UTF-8.
fn double_byte(
    raw: &[u8],
    spans: &[DoubleByteSpan],
    encoding: &'static encoding_rs::Encoding,
    charset: Charset,
) -> Option<Reading> {
    let strict = |bytes: &[u8]| {
        encoding
            .decode_without_bom_handling_and_without_replacement(bytes)
            .map(std::borrow::Cow::into_owned)
    };
    for span in spans {
        let bytes = raw.get(span.start..span.end)?;
        let text = strict(bytes)?;
        if text.chars().count() * 2 != bytes.len() {
            return None;
        }
    }
    for run in outside_runs(raw, spans) {
        strict(run)?;
        if !run.is_ascii() && std::str::from_utf8(run).is_ok() {
            return None;
        }
    }
    strict(raw).map(|text| Reading { text, charset })
}

/// The maximal runs of `raw` outside `spans` (spans in order, in bounds).
fn outside_runs<'a>(raw: &'a [u8], spans: &[DoubleByteSpan]) -> Vec<&'a [u8]> {
    let mut runs = Vec::new();
    let mut at = 0;
    for span in spans {
        let start = span.start.clamp(at, raw.len());
        if start > at {
            runs.push(&raw[at..start]);
        }
        at = span.end.clamp(start, raw.len());
    }
    if at < raw.len() {
        runs.push(&raw[at..]);
    }
    runs
}

/// Rules 1u and 1d: `unknown`; pairs inside the spans read in their own
/// set (WHATWG replacement for an ill-formed one), ASCII outside them stays,
/// every other byte becomes one U+FFFD. No spans: ASCII kept, the rest
/// marked.
fn mark_unread(raw: &[u8], spans: &[DoubleByteSpan]) -> Reading {
    let mut text = String::with_capacity(raw.len());
    let mut at = 0;
    for span in spans {
        let start = span.start.clamp(at, raw.len());
        let end = span.end.clamp(start, raw.len());
        push_ascii_or_marked(&mut text, &raw[at..start]);
        let encoding = match span.mode {
            DoubleByteMode::Kanji => encoding_rs::SHIFT_JIS,
            DoubleByteMode::Hanzi => encoding_rs::GB18030,
        };
        text.push_str(&encoding.decode_without_bom_handling(&raw[start..end]).0);
        at = end;
    }
    push_ascii_or_marked(&mut text, &raw[at..]);
    Reading {
        text,
        charset: Charset::Unknown,
    }
}

/// ASCII bytes as themselves, every other byte as one U+FFFD.
fn push_ascii_or_marked(text: &mut String, bytes: &[u8]) {
    for &b in bytes {
        text.push(if b.is_ascii() {
            char::from(b)
        } else {
            REPLACEMENT
        });
    }
}

/// Rule 1e: the engine's mode-aware reading chooses a complete decoding.
fn read_engine_reading(raw: &[u8], engine: EngineReading) -> Option<Reading> {
    let (encoding, charset) = match engine {
        EngineReading::None => return None,
        EngineReading::ShiftJis => (encoding_rs::SHIFT_JIS, Charset::ShiftJis),
        EngineReading::Gb18030 => (encoding_rs::GB18030, Charset::Gb18030),
    };
    let text = encoding.decode_without_bom_handling_and_without_replacement(raw)?;
    has_pair(raw, encoding).then(|| Reading {
        text: text.into_owned(),
        charset,
    })
}

/// Rule 1a: the legacy heuristic and its label.
fn legacy(raw: &[u8]) -> Reading {
    let (text, charset) = charset::resolve(raw);
    Reading { text, charset }
}

/// Rule 3: a designator signalled but not located — which bytes it governs
/// and under which set is unknown, so is the text: every byte marked.
fn read_signalled(raw: &[u8]) -> Reading {
    Reading {
        text: replacement(raw.len()),
        charset: Charset::Unknown,
    }
}

/// An interpretation governing bytes of a located content (rule 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Governs {
    /// Bytes before the first designator: ISO/IEC 8859-1.
    Default,
    /// A designator's span.
    Designator(u32),
}

/// Rule 2: located designators decide their bytes.
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
    let prefix = raw.get(..first).unwrap_or_default();
    let mut text = charset::iso_8859_1(prefix);
    let mut governing = Vec::with_capacity(marks.len() + 1);
    if !prefix.is_ascii() {
        governing.push(Governs::Default);
    }
    let mut failed = false;
    let ends = starts.iter().skip(1).copied().chain([raw.len()]);
    for ((mark, &start), end) in marks.iter().zip(&starts).zip(ends) {
        let bytes = raw.get(start..end).unwrap_or_default();
        // a designator whose span is empty governs no byte
        if bytes.is_empty() {
            continue;
        }
        governing.push(Governs::Designator(mark.value));
        if let Some(set) = eci_set(mark.value) {
            let (part, replaced) = decode(bytes, set);
            text.push_str(&part);
            failed |= replaced;
        } else {
            text.push_str(&replacement(bytes.len()));
            failed = true;
        }
    }
    let charset = if failed {
        Charset::Unknown
    } else {
        decoder_label(raw, &text, &governing).unwrap_or(Charset::Eci)
    };
    Reading { text, charset }
}

/// The first decoder label whose family holds every governing
/// interpretation and whose decoder over all of `raw` yields `text`.
fn decoder_label(raw: &[u8], text: &str, governing: &[Governs]) -> Option<Charset> {
    [
        Charset::Utf8,
        Charset::ShiftJis,
        Charset::Latin1,
        Charset::Gb18030,
    ]
    .into_iter()
    .find(|&label| {
        governing.iter().all(|&g| in_family(label, g))
            && charset::decode_legacy(raw, label).as_deref() == Some(text)
    })
}

/// The decoder-label families (spec/01 rule 2): `utf8` ← 26, 27 ·
/// `shift_jis` ← 20 · `latin1` ← 1, 3, 23 and the default · `gb18030` ← 29,
/// 31, 32.
fn in_family(label: Charset, governs: Governs) -> bool {
    match governs {
        Governs::Default => label == Charset::Latin1,
        Governs::Designator(value) => matches!(
            (label, value),
            (Charset::Utf8, 26 | 27)
                | (Charset::ShiftJis, 20)
                | (Charset::Latin1, 1 | 3 | 23)
                | (Charset::Gb18030, 29 | 31 | 32)
        ),
    }
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
    use crate::report::EngineKind;

    fn located(marks: &[(u32, usize)]) -> EciEvidence {
        EciEvidence::Located(
            marks
                .iter()
                .map(|&(value, start)| EciMark { value, start })
                .collect(),
        )
    }

    /// The reading of a QR content under `eci`, its segments not observed.
    fn qr(raw: &[u8], eci: &EciEvidence) -> Reading {
        read(raw, eci, &Segments::NotObserved, EngineReading::None, true)
    }

    /// The reading of a parsed, unmarked QR content with these spans.
    fn parsed(raw: &[u8], spans: &[(DoubleByteMode, usize, usize)]) -> Reading {
        let spans = spans
            .iter()
            .map(|&(mode, start, end)| DoubleByteSpan { mode, start, end })
            .collect();
        read(
            raw,
            &EciEvidence::NoEci,
            &Segments::Observed(spans),
            EngineReading::None,
            true,
        )
    }

    fn pair(reading: &Reading) -> (&str, Charset) {
        (reading.text.as_str(), reading.charset)
    }

    const KONNICHIWA_SJIS: [u8; 10] = [0x82, 0xB1, 0x82, 0xF1, 0x82, 0xC9, 0x82, 0xBF, 0x82, 0xCD];

    /// (raw, designators as (value, start), text, charset)
    type Pin = (
        &'static [u8],
        &'static [(u32, usize)],
        &'static str,
        Charset,
    );

    #[test]
    fn located_designators_decide_their_bytes_and_the_label() {
        // Each pin: a decoder label only when its family holds every governing
        // interpretation and its decoder reproduces the text; else `eci`;
        // `unknown` only when a governed byte failed
        let pins: [Pin; 15] = [
            (b"hello", &[(4, 0)], "hello", Charset::Eci),
            (b"ok", &[(27, 0)], "ok", Charset::Utf8),
            (&[0x00, 0x41, 0x00, 0x42], &[(25, 0)], "AB", Charset::Eci),
            (
                b"hello",
                &[(170, 0)],
                "\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}",
                Charset::Unknown,
            ),
            (
                &[0xA1, 0xA2, 0xA3, 0xA4, 0xA5],
                &[(9, 0)],
                "‘’£€₯",
                Charset::Eci,
            ),
            (
                b"caf\xC3\xA9\xFF",
                &[(26, 0)],
                "café\u{FFFD}",
                Charset::Unknown,
            ),
            (
                &[0xF0, 0x9F, 0x98, 0x41],
                &[(26, 0)],
                "\u{FFFD}A",
                Charset::Unknown,
            ),
            (
                &[0xA1, 0x41, 0x42],
                &[(4, 0), (999, 1)],
                "Ą\u{FFFD}\u{FFFD}",
                Charset::Unknown,
            ),
            (&[0xE9, 0xC3, 0xA9], &[(26, 1)], "éé", Charset::Eci),
            (&[0xE9, 0xE9], &[(3, 1)], "éé", Charset::Latin1),
            (
                &[0x93, 0xFA, 0x96, 0x7B],
                &[(20, 2)],
                "\u{93}ú本",
                Charset::Eci,
            ),
            (&[0xC3, 0xA9], &[(31, 0)], "茅", Charset::Gb18030),
            (
                &[0xD6, 0xD0, 0xCE, 0xC4],
                &[(29, 0)],
                "中文",
                Charset::Gb18030,
            ),
            (
                &[0xD6, 0xD0, 0xCE, 0xC4, 0x95, 0x32, 0x82, 0x36],
                &[(32, 0)],
                "中文𠀀",
                Charset::Gb18030,
            ),
            (
                &[0xE9, 0xD6, 0xD0, 0xCE, 0xC4],
                &[(29, 1)],
                "é中文",
                Charset::Eci,
            ),
        ];
        for (i, (raw, marks, text, charset)) in pins.into_iter().enumerate() {
            let reading = qr(raw, &located(marks));
            assert_eq!(pair(&reading), (text, charset), "P{}", i + 1);
        }
        let privet = [0xBF, 0xE0, 0xD8, 0xD2, 0xD5, 0xE2];
        assert_eq!(qr(&privet, &located(&[(7, 0)])).text, "Привет");
    }

    #[test]
    fn a_decoder_label_requires_its_family_and_an_exact_reproduction() {
        let cases: [Pin; 7] = [
            ("\u{104}".as_bytes(), &[(26, 0)], "\u{104}", Charset::Utf8),
            (b"A", &[(27, 0)], "A", Charset::Utf8),
            (
                &KONNICHIWA_SJIS,
                &[(20, 0)],
                "こんにちは",
                Charset::ShiftJis,
            ),
            (&[0xA1], &[(3, 0)], "¡", Charset::Latin1),
            (&[0x80], &[(23, 0)], "€", Charset::Latin1),
            // ISO/IEC 8859-1 keeps 0x80 a C1 control; the latin1 label's
            // decoder reads '€' — no reproduction, so `eci`
            (&[0x35, 0x80], &[(3, 0)], "5\u{80}", Charset::Eci),
            // two families (latin1 + utf8): no single decoder label
            (b"a\xC3\xA9", &[(3, 0), (26, 1)], "aé", Charset::Eci),
        ];
        for (raw, marks, text, charset) in cases {
            assert_eq!(
                pair(&qr(raw, &located(marks))),
                (text, charset),
                "{marks:?}"
            );
        }
    }

    #[test]
    fn each_designator_governs_until_the_next() {
        let raw = [0xA1, 0xE2, 0x82, 0xAC];
        let reading = qr(&raw, &located(&[(4, 0), (26, 1)]));
        assert_eq!(pair(&reading), ("\u{104}€", Charset::Eci));
    }

    #[test]
    fn every_byte_before_the_first_designator_is_iso_8859_1() {
        let reading = qr(b"12345\xA1", &located(&[(4, 5)]));
        assert_eq!(reading.text, "12345\u{104}");
        // Kanji pairs there too: transmission-faithful, never Shift JIS
        let mut raw = KONNICHIWA_SJIS[..2].to_vec();
        raw.push(b'a');
        let reading = qr(&raw, &located(&[(26, 2)]));
        assert_eq!(
            pair(&reading),
            ("\u{82}±a", Charset::Eci),
            "decoded, not marked"
        );
    }

    #[test]
    fn undecodable_bytes_are_marked_never_transcribed() {
        // inside a supported set: one U+FFFD per maximal ill-formed subpart
        let reading = qr(&[0xA1], &located(&[(26, 0)]));
        assert_eq!(pair(&reading), ("\u{FFFD}", Charset::Unknown));
        let mut raw = "é".as_bytes().to_vec();
        raw.extend_from_slice(&KONNICHIWA_SJIS);
        let reading = qr(&raw, &located(&[(26, 0)]));
        assert_eq!(reading.text, String::from_utf8_lossy(&raw));
        // binary and designators outside the table: one U+FFFD per byte
        for value in [899, 0, 2, 14, 19, 170, 999, 999_999] {
            let reading = qr(&[0x00, 0xFF], &located(&[(value, 0)]));
            assert_eq!(
                pair(&reading),
                ("\u{FFFD}\u{FFFD}", Charset::Unknown),
                "ECI {value}"
            );
        }
        // decodable segments keep their reading next to a marked one
        let reading = qr(b"ab\xA1", &located(&[(26, 0), (999, 2)]));
        assert_eq!(pair(&reading), ("ab\u{FFFD}", Charset::Unknown));
    }

    #[test]
    fn a_designator_governing_no_byte_never_decides_the_label() {
        // Each case: an empty span neither decodes nor affects the label
        let cases: [Pin; 8] = [
            (b"", &[(999, 0)], "", Charset::Utf8),
            (b"caf\xC3\xA9", &[(26, 0), (999, 5)], "café", Charset::Utf8),
            (b"caf\xC3\xA9", &[(26, 0), (4, 5)], "café", Charset::Utf8),
            (b"ok", &[(999, 0), (26, 0)], "ok", Charset::Utf8),
            (b"ok", &[(899, 0), (26, 0)], "ok", Charset::Utf8),
            // never shift_jis or latin1: windows-1252 reads 0x80 as '€'
            (&[0x80], &[(999, 1)], "\u{80}", Charset::Eci),
            (b"12345\xC3\xA9", &[(26, 5)], "12345é", Charset::Utf8),
            (&[0x35, 0x80], &[(3, 0)], "5\u{80}", Charset::Eci),
        ];
        for (i, (raw, marks, text, charset)) in cases.into_iter().enumerate() {
            assert_eq!(
                pair(&qr(raw, &located(marks))),
                (text, charset),
                "Z{}",
                i + 1
            );
        }
        // empty raw is ("", utf8) under every rule
        for eci in [
            EciEvidence::NoEci,
            EciEvidence::Signalled,
            EciEvidence::Unobserved,
        ] {
            assert_eq!(pair(&qr(b"", &eci)), ("", Charset::Utf8), "{eci:?}");
        }
    }

    #[test]
    fn signalled_only_content_is_marked_unknown() {
        // every byte one U+FFFD, whatever the heuristic would pick
        let gs1 = b"0104012345678901";
        for raw in [
            &[0xA1][..],
            &[0x93, 0xFA, 0x96, 0x7B],
            &[0xC3, 0xA9],
            "café".as_bytes(),
            gs1,
        ] {
            let reading = qr(raw, &EciEvidence::Signalled);
            assert_eq!(
                pair(&reading),
                (replacement(raw.len()).as_str(), Charset::Unknown),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn unobserved_qr_reads_mark_what_they_cannot_check() {
        // a QR read whose declarations the scanner could not observe
        for (raw, text, charset) in [
            (&[0xA1][..], "\u{FFFD}", Charset::Unknown),
            (b"Ab\xE9", "Ab\u{FFFD}", Charset::Unknown),
            ("café".as_bytes(), "caf\u{FFFD}\u{FFFD}", Charset::Unknown),
            (b"abc", "abc", Charset::Utf8),
        ] {
            let reading = qr(raw, &EciEvidence::Unobserved);
            assert_eq!(pair(&reading), (text, charset), "{raw:?}");
        }
        // outside the QR family the legacy reading stands
        let reading = read(
            &[0xE9],
            &EciEvidence::Unobserved,
            &Segments::NotObserved,
            EngineReading::None,
            false,
        );
        assert_eq!(pair(&reading), ("é", Charset::Latin1));
    }

    #[test]
    fn the_engine_reading_only_chooses_a_complete_decoding() {
        // segments not observed
        let yamada = [0x8E, 0x52, 0x93, 0x63];
        let chuwen = [0xD6, 0xD0, 0xCE, 0xC4];
        let reading = |raw: &[u8], engine| {
            read(
                raw,
                &EciEvidence::NoEci,
                &Segments::NotObserved,
                engine,
                true,
            )
        };
        let cases = [
            (&yamada, EngineReading::ShiftJis, "山田", Charset::ShiftJis),
            (&yamada, EngineReading::None, "ŽR“c", Charset::Latin1),
            (&chuwen, EngineReading::Gb18030, "中文", Charset::Gb18030),
            // the legacy residue: four half-width katakana
            (&chuwen, EngineReading::None, "ﾖﾐﾎﾄ", Charset::ShiftJis),
        ];
        for (raw, engine, text, charset) in cases {
            assert_eq!(pair(&reading(raw, engine)), (text, charset), "{engine:?}");
        }
        // the derivation from the engine's text: a complete decoding with a
        // pair, never a single-byte one or another reading
        assert_eq!(EngineReading::of(&yamada, "山田"), EngineReading::ShiftJis);
        assert_eq!(EngineReading::of(&chuwen, "中文"), EngineReading::Gb18030);
        assert_eq!(
            EngineReading::of(&[0x35, 0x80], "5\u{80}"),
            EngineReading::None
        );
        assert_eq!(
            EngineReading::of(b"Qu\xE9bec", "Québec"),
            EngineReading::None
        );
        assert_eq!(EngineReading::of("é".as_bytes(), "é"), EngineReading::None);
        // observed segments decide: the engine reading is not consulted
        let observed = read(
            &yamada,
            &EciEvidence::NoEci,
            &Segments::Observed(Vec::new()),
            EngineReading::ShiftJis,
            true,
        );
        assert_eq!(pair(&observed), ("ŽR“c", Charset::Latin1));
    }

    #[test]
    fn observed_segments_choose_the_double_byte_decoding() {
        use DoubleByteMode::{Hanzi, Kanji};
        // Kanji segments: Shift_JIS over all of raw (1b)
        let yamada = [0x8E, 0x52, 0x93, 0x63];
        assert_eq!(
            pair(&parsed(&yamada, &[(Kanji, 0, 4)])),
            ("山田", Charset::ShiftJis)
        );
        // Hanzi segments: gb18030 (1c)
        let chuwen = [0xD6, 0xD0, 0xCE, 0xC4];
        assert_eq!(
            pair(&parsed(&chuwen, &[(Hanzi, 0, 4)])),
            ("中文", Charset::Gb18030)
        );
        // byte data outside that is UTF-8 with a byte from 0x80: 1d
        let mut mixed = b"\xC3\xA9".to_vec();
        mixed.extend_from_slice(&yamada);
        assert_eq!(
            pair(&parsed(&mixed, &[(Kanji, 2, 6)])),
            ("\u{FFFD}\u{FFFD}山田", Charset::Unknown)
        );
        // Kanji and Hanzi together: each pair in its own set (1d)
        let mut both = yamada.to_vec();
        both.extend_from_slice(b"-");
        both.extend_from_slice(&chuwen);
        assert_eq!(
            pair(&parsed(&both, &[(Kanji, 0, 4), (Hanzi, 5, 9)])),
            ("山田-中文", Charset::Unknown)
        );
        // conflicting observations: no pair read
        let conflicting = read(
            &yamada,
            &EciEvidence::NoEci,
            &Segments::Conflicting,
            EngineReading::ShiftJis,
            true,
        );
        assert_eq!(pair(&conflicting), ("\u{FFFD}R\u{FFFD}c", Charset::Unknown));
        // no segment: the legacy reading (1a)
        assert_eq!(pair(&parsed(&yamada, &[])), ("ŽR“c", Charset::Latin1));
    }

    #[test]
    fn ill_formed_pairs_get_the_whatwg_replacement() {
        use DoubleByteMode::{Hanzi, Kanji};
        // a lead with an ASCII trail is U+FFFD then that character
        assert_eq!(
            pair(&parsed(&[0x81, 0x7F], &[(Kanji, 0, 2)])),
            ("\u{FFFD}\u{7F}", Charset::Unknown)
        );
        // a lead with a non-ASCII bad trail is one U+FFFD
        assert_eq!(
            pair(&parsed(&[0x9F, 0xFD], &[(Kanji, 0, 2)])),
            ("\u{FFFD}", Charset::Unknown)
        );
        assert_eq!(
            pair(&parsed(&[0xB0, 0xFF], &[(Hanzi, 0, 2)])),
            ("\u{FFFD}", Charset::Unknown)
        );
        // a Kanji span over valid UTF-8: utf8
        assert_eq!(
            pair(&parsed("é".as_bytes(), &[(Kanji, 0, 2)])),
            ("é", Charset::Utf8)
        );
    }

    #[test]
    fn iso_8859_9_and_11_are_exact_in_crate_tables() {
        // 8859-9: C1 controls stay controls; the A0-FF half equals the
        // windows-1254 superset (independent oracle)
        let reading = qr(&[0xD0, 0x80], &located(&[(11, 0)]));
        assert_eq!(reading.text, "Ğ\u{80}");
        let upper: Vec<u8> = (0xA0..=0xFF).collect();
        let (oracle, _) = encoding_rs::WINDOWS_1254.decode_without_bom_handling(&upper);
        assert_eq!(qr(&upper, &located(&[(11, 0)])).text, oracle);
        // 8859-11: Thai; DB-DE and FC-FF unassigned
        assert_eq!(qr(&[0xA1, 0xDF], &located(&[(13, 0)])).text, "ก฿");
        let reading = qr(&[0xDB], &located(&[(13, 0)]));
        assert_eq!(pair(&reading), ("\u{FFFD}", Charset::Unknown));
        let assigned: Vec<u8> = (0xA0..=0xFB)
            .filter(|b| !(0xDB..=0xDE).contains(b))
            .collect();
        let (oracle, _) = encoding_rs::WINDOWS_874.decode_without_bom_handling(&assigned);
        assert_eq!(qr(&assigned, &located(&[(13, 0)])).text, oracle);
        assert_eq!(qr(&[0x80], &located(&[(13, 0)])).text, "\u{80}");
    }

    #[test]
    fn the_rest_of_the_decode_table() {
        // GB 2312 / GBK / GB 18030 through the gb18030 decoder
        for value in [29, 31, 32] {
            assert_eq!(qr(&[0xB0, 0xA1], &located(&[(value, 0)])).text, "啊");
        }
        assert_eq!(qr(&[0xA4, 0x40], &located(&[(28, 0)])).text, "一");
        assert_eq!(qr(&[0xB0, 0xA1], &located(&[(30, 0)])).text, "가");
        assert_eq!(qr(&[0xC1], &located(&[(21, 0)])).text, "Á");
        assert_eq!(qr(&[0xC0], &located(&[(22, 0)])).text, "А");
        assert_eq!(qr(&[0xC7], &located(&[(24, 0)])).text, "ا");
        assert_eq!(qr(&[0x00, 0x41], &located(&[(25, 0)])).text, "A");
        assert_eq!(qr(&[0x41, 0x00], &located(&[(33, 0)])).text, "A");
        // an odd UTF-16 byte is one marked subpart
        let reading = qr(&[0x00, 0x41, 0x00], &located(&[(25, 0)]));
        assert_eq!(pair(&reading), ("A\u{FFFD}", Charset::Unknown));
        // UTF-32 in-crate: a surrogate and a truncated unit are marked
        assert_eq!(qr(&[0, 0, 0, 0x41], &located(&[(34, 0)])).text, "A");
        assert_eq!(qr(&[0x41, 0, 0, 0], &located(&[(35, 0)])).text, "A");
        let reading = qr(&[0, 0, 0xD8, 0, 0, 0], &located(&[(34, 0)]));
        assert_eq!(pair(&reading), ("\u{FFFD}\u{FFFD}", Charset::Unknown));
        // ASCII: bytes from 0x80 have no reading
        let reading = qr(&[0x41, 0xA1], &located(&[(27, 0)]));
        assert_eq!(pair(&reading), ("A\u{FFFD}", Charset::Unknown));
        // the WHATWG decoders never sniff a byte-order mark
        assert_eq!(
            qr(&[0xEF, 0xBB, 0xBF, 0x41], &located(&[(26, 0)])).text,
            "\u{FEFF}A"
        );
    }

    #[test]
    fn the_decode_table_is_spec_01_s() {
        use encoding_rs as e;
        // spec/01 § ECI decode table, every row with a character set; every
        // other assignment number has none (899 binary, 0 and 2 CP437, 14,
        // 19, 170 …)
        let table: [(&[u32], EciSet); 29] = [
            (&[1, 3], EciSet::Latin1),
            (&[4], EciSet::Whatwg(e::ISO_8859_2)),
            (&[5], EciSet::Whatwg(e::ISO_8859_3)),
            (&[6], EciSet::Whatwg(e::ISO_8859_4)),
            (&[7], EciSet::Whatwg(e::ISO_8859_5)),
            (&[8], EciSet::Whatwg(e::ISO_8859_6)),
            (&[9], EciSet::Whatwg(e::ISO_8859_7)),
            (&[10], EciSet::Whatwg(e::ISO_8859_8)),
            (&[11], EciSet::Iso8859_9),
            (&[12], EciSet::Whatwg(e::ISO_8859_10)),
            (&[13], EciSet::Iso8859_11),
            (&[15], EciSet::Whatwg(e::ISO_8859_13)),
            (&[16], EciSet::Whatwg(e::ISO_8859_14)),
            (&[17], EciSet::Whatwg(e::ISO_8859_15)),
            (&[18], EciSet::Whatwg(e::ISO_8859_16)),
            (&[20], EciSet::Whatwg(e::SHIFT_JIS)),
            (&[21], EciSet::Whatwg(e::WINDOWS_1250)),
            (&[22], EciSet::Whatwg(e::WINDOWS_1251)),
            (&[23], EciSet::Whatwg(e::WINDOWS_1252)),
            (&[24], EciSet::Whatwg(e::WINDOWS_1256)),
            (&[25], EciSet::Whatwg(e::UTF_16BE)),
            (&[26], EciSet::Whatwg(e::UTF_8)),
            (&[27], EciSet::Ascii),
            (&[28], EciSet::Whatwg(e::BIG5)),
            (&[29, 31, 32], EciSet::Whatwg(e::GB18030)),
            (&[30], EciSet::Whatwg(e::EUC_KR)),
            (&[33], EciSet::Whatwg(e::UTF_16LE)),
            (&[34], EciSet::Utf32 { big_endian: true }),
            (&[35], EciSet::Utf32 { big_endian: false }),
        ];
        for value in (0..=1000).chain([999_999]) {
            let expected = table
                .iter()
                .find(|(values, _)| values.contains(&value))
                .map(|&(_, set)| set);
            assert_eq!(eci_set(value), expected, "ECI {value}");
        }
        // and each designator's bytes read in its own set: a probe no other
        // single-byte set of the table reads alike (Python's codecs as the
        // oracle), the label `eci` — no decoder label's family holds them
        for (value, probe, text) in [
            (5, &[0xA1][..], "\u{126}"),
            (6, &[0xA2], "\u{138}"),
            (8, &[0xAC], "\u{60C}"),
            (10, &[0xAA], "\u{D7}"),
            (12, &[0xA2], "\u{112}"),
            (15, &[0xA1], "\u{201D}"),
            (16, &[0xA1], "\u{1E02}"),
            (17, &[0xA4, 0xA1], "\u{20AC}\u{A1}"),
            (18, &[0xA2], "\u{105}"),
        ] {
            let reading = qr(probe, &located(&[(value, 0)]));
            assert_eq!(pair(&reading), (text, Charset::Eci), "ECI {value}");
        }
    }

    #[test]
    fn a_gb18030_four_byte_sequence_is_no_pair() {
        // 81 30 81 30 is U+0080: a well-formed gb18030 reading without a byte
        // pair, so not the complete double-byte decoding rule 1e chooses
        let four = [0x41, 0x81, 0x30, 0x81, 0x30];
        assert!(!has_pair(&four, encoding_rs::GB18030));
        assert_eq!(EngineReading::of(&four, "A\u{80}"), EngineReading::None);
        // a pair after a four-byte sequence still counts
        let then_pair = [0x41, 0x41, 0x81, 0x30, 0x81, 0x30, 0xB0, 0xA1];
        assert!(has_pair(&then_pair, encoding_rs::GB18030));
        assert_eq!(
            EngineReading::of(&then_pair, "AA\u{80}啊"),
            EngineReading::Gb18030
        );
    }

    #[test]
    fn the_runs_outside_the_spans_are_maximal_and_never_empty() {
        let span = |start, end| DoubleByteSpan {
            mode: DoubleByteMode::Kanji,
            start,
            end,
        };
        let raw = b"abcdefgh";
        let runs = |spans: &[DoubleByteSpan]| -> Vec<Vec<u8>> {
            outside_runs(raw, spans)
                .into_iter()
                .map(<[u8]>::to_vec)
                .collect()
        };
        assert_eq!(runs(&[]), [b"abcdefgh".to_vec()]);
        assert_eq!(
            runs(&[span(1, 3), span(5, 7)]),
            [b"a".to_vec(), b"de".to_vec(), b"h".to_vec()]
        );
        // adjacent spans, and spans at either end, leave no empty run
        assert!(runs(&[span(0, 2), span(2, 4), span(4, 8)]).is_empty());
        assert_eq!(runs(&[span(0, 4)]), [b"efgh".to_vec()]);
        assert_eq!(runs(&[span(4, 8)]), [b"abcd".to_vec()]);
        // a trailing run of UTF-8 after Kanji: rule 1d, as a leading one is
        let mut trailing = [0x8E, 0x52, 0x93, 0x63].to_vec();
        trailing.extend_from_slice("é".as_bytes());
        assert_eq!(
            pair(&parsed(&trailing, &[(DoubleByteMode::Kanji, 0, 4)])),
            ("山田\u{FFFD}\u{FFFD}", Charset::Unknown)
        );
    }

    #[test]
    fn evidence_lattice() {
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
        assert_eq!(EciEvidence::from_marks(Vec::new()), none);
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
        let hit = |raw: &[u8], eci: EciEvidence, fnc1: Fnc1| {
            RawDetection::qr(raw, eci, fnc1, EngineKind::Rxing)
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
        // between keys: the same test, symmetric, and a hit's own key
        let (a1, gs1_bytes): (&[u8], &[u8]) = (&[0xA1], b"10AB");
        for (raw, eci, fnc1) in [
            (a1, EciEvidence::Signalled, Fnc1::None),
            (a1, EciEvidence::NoEci, Fnc1::None),
            (a1, EciEvidence::Unobserved, Fnc1::Unobserved),
            (gs1_bytes, EciEvidence::NoEci, Fnc1::First),
            (gs1_bytes, EciEvidence::NoEci, Fnc1::Second),
        ] {
            let read = hit(raw, eci, fnc1);
            let own = ContentKey::of(&read);
            assert!(own.admits(&read));
            for other in [&key, &gs1] {
                assert_eq!(other.agrees(&own), other.admits(&read), "{own:?}");
                assert_eq!(own.agrees(other), other.agrees(&own), "symmetric");
            }
        }
    }

    #[test]
    fn inconsistent_offsets_never_panic() {
        let reading = qr(b"ab", &located(&[(26, 5)]));
        assert_eq!(
            pair(&reading),
            ("ab", Charset::Utf8),
            "clamped: all before it"
        );
        let reading = qr(b"ab", &located(&[(26, 1), (4, 0)]));
        assert_eq!(reading.text, "ab");
        let spans = [DoubleByteSpan {
            mode: DoubleByteMode::Kanji,
            start: 3,
            end: 1,
        }];
        let reading = read(
            b"\x93\xFA",
            &EciEvidence::NoEci,
            &Segments::Observed(spans.to_vec()),
            EngineReading::None,
            true,
        );
        assert_eq!(reading.text.chars().count(), 2);
    }

    fn evidence_strategy() -> impl Strategy<Value = EciEvidence> {
        prop_oneof![
            Just(EciEvidence::Unobserved),
            Just(EciEvidence::NoEci),
            Just(EciEvidence::Signalled),
            proptest::collection::vec(mark_strategy(), 1..4).prop_map(EciEvidence::Located),
        ]
    }

    fn mark_strategy() -> impl Strategy<Value = EciMark> {
        (
            prop_oneof![
                Just(3u32),
                Just(4),
                Just(20),
                Just(26),
                Just(29),
                Just(899),
                Just(999),
                0u32..1000
            ],
            0usize..14,
        )
            .prop_map(|(value, start)| EciMark { value, start })
    }

    fn segments_strategy() -> impl Strategy<Value = Segments> {
        let span =
            (any::<bool>(), 0usize..14, 0usize..14).prop_map(|(kanji, a, b)| DoubleByteSpan {
                mode: if kanji {
                    DoubleByteMode::Kanji
                } else {
                    DoubleByteMode::Hanzi
                },
                start: a.min(b),
                end: a.max(b),
            });
        prop_oneof![
            Just(Segments::NotObserved),
            Just(Segments::Conflicting),
            proptest::collection::vec(span, 0..3).prop_map(Segments::Observed),
        ]
    }

    fn engine_strategy() -> impl Strategy<Value = EngineReading> {
        prop_oneof![
            Just(EngineReading::None),
            Just(EngineReading::ShiftJis),
            Just(EngineReading::Gb18030),
        ]
    }

    /// One piece of a parsed payload: an ASCII byte or a well-formed pair.
    #[derive(Debug, Clone, Copy)]
    enum Piece {
        Ascii(u8),
        Kanji(char),
        Hanzi(char),
    }

    fn piece_strategy() -> impl Strategy<Value = Piece> {
        prop_oneof![
            (0x20u8..0x7F).prop_map(Piece::Ascii),
            prop::sample::select(vec!['日', '本', '山', '田', '漢', '字', 'テ'])
                .prop_map(Piece::Kanji),
            prop::sample::select(vec!['中', '文', '汉', '字', '国', '啊']).prop_map(Piece::Hanzi),
        ]
    }

    proptest! {
        /// Global invariants of the one interpretation over arbitrary
        /// (even inconsistent) observations: empty text iff empty bytes,
        /// deterministic, and a decoder label always reproduces the text
        /// from the bytes exactly.
        #[test]
        fn readings_are_total_deterministic_and_label_consistent(
            raw in proptest::collection::vec(any::<u8>(), 0..12),
            evidence in evidence_strategy(),
            segments in segments_strategy(),
            engine in engine_strategy(),
            qr_family in any::<bool>(),
        ) {
            let reading = read(&raw, &evidence, &segments, engine, qr_family);
            prop_assert_eq!(raw.is_empty(), reading.text.is_empty());
            prop_assert_eq!(&reading, &read(&raw, &evidence, &segments, engine, qr_family));
            if matches!(
                reading.charset,
                Charset::Utf8 | Charset::ShiftJis | Charset::Latin1 | Charset::Gb18030
            ) {
                let decoded = charset::decode_legacy(&raw, reading.charset);
                prop_assert_eq!(decoded.as_deref(), Some(reading.text.as_str()));
            }
            if reading.charset == Charset::Eci {
                prop_assert!(matches!(evidence, EciEvidence::Located(_)));
            }
        }

        /// No double-byte segment: the legacy reading, label included.
        #[test]
        fn without_a_segment_the_legacy_reading_stands(
            raw in proptest::collection::vec(any::<u8>(), 0..12),
        ) {
            let reading = read(
                &raw,
                &EciEvidence::NoEci,
                &Segments::Observed(Vec::new()),
                EngineReading::None,
                true,
            );
            let (text, charset) = charset::resolve(&raw);
            prop_assert_eq!((reading.text, reading.charset), (text, charset));
        }

        /// Every well-formed pair inside a span is one character, and ASCII
        /// outside the spans stays itself.
        #[test]
        fn every_well_formed_pair_is_one_character(
            pieces in proptest::collection::vec(piece_strategy(), 1..8),
        ) {
            let mut raw = Vec::new();
            let mut spans: Vec<DoubleByteSpan> = Vec::new();
            let mut pairs = 0;
            for piece in &pieces {
                let (encoding, mode, c) = match *piece {
                    Piece::Ascii(b) => {
                        raw.push(b);
                        continue;
                    }
                    Piece::Kanji(c) => (encoding_rs::SHIFT_JIS, DoubleByteMode::Kanji, c),
                    Piece::Hanzi(c) => (encoding_rs::GB18030, DoubleByteMode::Hanzi, c),
                };
                let mut utf8 = [0; 4];
                let (bytes, _, _) = encoding.encode(c.encode_utf8(&mut utf8));
                prop_assert_eq!(bytes.len(), 2);
                let start = raw.len();
                raw.extend_from_slice(&bytes);
                pairs += 1;
                match spans.last_mut() {
                    Some(last) if last.mode == mode && last.end == start => last.end = raw.len(),
                    _ => spans.push(DoubleByteSpan { mode, start, end: raw.len() }),
                }
            }
            let ascii = pieces.iter().filter(|p| matches!(p, Piece::Ascii(_))).count();
            let reading = read(
                &raw,
                &EciEvidence::NoEci,
                &Segments::Observed(spans),
                EngineReading::None,
                true,
            );
            if reading.charset != Charset::Utf8 {
                prop_assert_eq!(reading.text.chars().count(), ascii + pairs);
                prop_assert!(!reading.text.contains(REPLACEMENT));
            }
        }

        /// Sp: an unlocated declaration marks every byte.
        #[test]
        fn a_signalled_declaration_marks_every_byte(
            raw in proptest::collection::vec(any::<u8>(), 1..12),
        ) {
            let reading = qr(&raw, &EciEvidence::Signalled);
            prop_assert_eq!(reading.charset, Charset::Unknown);
            prop_assert_eq!(reading.text.chars().count(), raw.len());
            prop_assert!(reading.text.chars().all(|c| c == REPLACEMENT));
        }

        /// Zp: removing a designator whose span is empty, while another
        /// remains, changes neither the text nor the label.
        #[test]
        fn an_empty_span_designator_changes_nothing(
            raw in proptest::collection::vec(any::<u8>(), 0..12),
            marks in proptest::collection::vec(mark_strategy(), 2..5),
        ) {
            let mut marks = marks;
            marks.sort_by_key(|mark| mark.start);
            for mark in &mut marks {
                mark.start = mark.start.min(raw.len());
            }
            let reading = qr(&raw, &EciEvidence::Located(marks.clone()));
            for i in 0..marks.len() {
                let end = marks.get(i + 1).map_or(raw.len(), |next| next.start);
                if marks[i].start == end {
                    let mut fewer = marks.clone();
                    fewer.remove(i);
                    prop_assert_eq!(&qr(&raw, &EciEvidence::Located(fewer)), &reading);
                }
            }
        }
    }
}
