//! Corrected data codewords → payload bytes + segment structure (ISO/IEC
//! 18004 §7.4) — the ONE segment parser of the crate. Three observation
//! paths share it: the S5 rescue (its stream never reaches an engine), the
//! rqrr re-derivation (rqrr parses ECI designators and throws them away)
//! and rxing's general reader (whose raw bytes ARE data codewords). The
//! engines expose payload bytes at best; which ECI governs which bytes and
//! the FNC1 position only exist here. Differentially pinned against rqrr's
//! bytes on every clean corpus fixture.
//!
//! Refusal-biased: a structure ISO 18004 does not define (a `111xxxxx`
//! designator, an assignment number above 999999, an unknown mode, an
//! out-of-range numeric/alphanumeric value, an invalid AIM application
//! indicator, a truncated count) returns `None` — never a best guess.
//! [`port_payload`] names what a more tolerant reader transmits for such a
//! stream: the bytes a refusal must veto in that reader's output.

use crate::engine::content::{EciMark, Fnc1};

/// The rules a parse applies to designators and FNC1 indicators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rules {
    /// ISO/IEC 18004 exactly — the scanner's own reading.
    Iso,
    /// What rxing's zxing-cpp port also accepts (0.9.1): an assignment
    /// number above 999999 (read as "unknown") and FNC1 in both positions
    /// — second after first while no data precedes it, first after second
    /// (the last one wins). Never a reading: only the bytes such a stream
    /// transmits through that reader.
    Port,
}

/// MSB-first bit reader over the data codewords.
struct Bits<'a> {
    bytes: &'a [u8],
    cursor: usize, // bit index
}

impl Bits<'_> {
    fn remaining(&self) -> usize {
        self.bytes.len() * 8 - self.cursor
    }

    fn take(&mut self, n: usize) -> Option<u32> {
        if n > 24 || self.remaining() < n {
            return None;
        }
        let mut out = 0u32;
        for _ in 0..n {
            let byte = self.bytes[self.cursor / 8];
            let bit = (byte >> (7 - (self.cursor % 8))) & 1;
            out = (out << 1) | u32::from(bit);
            self.cursor += 1;
        }
        Some(out)
    }
}

/// Character-count field width per mode class and version (ISO table 3;
/// Hanzi per GB/T 18284 shares the Kanji widths).
fn count_bits(version: u8, mode: u32) -> Option<usize> {
    let class = match version {
        1..=9 => 0,
        10..=26 => 1,
        27..=40 => 2,
        _ => return None,
    };
    Some(match mode {
        0b0001 => [10, 12, 14][class],         // numeric
        0b0010 => [9, 11, 13][class],          // alphanumeric
        0b0100 => [8, 16, 16][class],          // byte
        0b1000 | 0b1101 => [8, 10, 12][class], // kanji · hanzi
        _ => return None,
    })
}

const ALPHANUMERIC: &[u8; 45] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:";

/// Group separator — what an FNC1-mode `%` transmits (ISO 18004 §7.4.8.2).
const GS: u8 = 0x1D;

/// One parsed data-segment stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parsed {
    /// Payload bytes as transmitted: numeric/alphanumeric as ASCII, byte
    /// mode verbatim, Kanji as Shift JIS pairs and Hanzi as GB 2312 pairs
    /// whatever ECI applies, the FNC1-second application indicator first.
    /// ECI designators and headers are NOT payload.
    pub(crate) bytes: Vec<u8>,
    /// ECI designators, in stream order, with the payload offset where each
    /// takes effect.
    pub(crate) eci: Vec<EciMark>,
    /// The declared FNC1 mode.
    pub(crate) fnc1: Fnc1,
    /// A structured-append header was present: this symbol is one part of
    /// a longer message (consumed, never reassembled).
    pub(crate) structured_append: bool,
    /// A Hanzi (GB/T 18284) segment was present.
    pub(crate) hanzi: bool,
}

/// Parse the data segment stream; `None` on any structure ISO 18004 does
/// not define (a rescue that corrected to garbage must not surface, and a
/// re-derivation must not invent a declaration).
pub(crate) fn parse(data: &[u8], version: u8) -> Option<Parsed> {
    parse_with(data, version, Rules::Iso)
}

/// The payload rxing's zxing-cpp port transmits for a stream [`parse`]
/// refuses — an assignment number above 999999, FNC1 indicators in both
/// positions. `None` when that reader refuses the stream too. A refusal
/// vetoes the port's read of exactly these bytes.
pub(crate) fn port_payload(data: &[u8], version: u8) -> Option<Vec<u8>> {
    parse_with(data, version, Rules::Port).map(|parsed| parsed.bytes)
}

fn parse_with(data: &[u8], version: u8, rules: Rules) -> Option<Parsed> {
    let mut bits = Bits {
        bytes: data,
        cursor: 0,
    };
    let mut out = Vec::new();
    let mut eci = Vec::new();
    let mut fnc1 = Fnc1::None;
    let mut structured_append = false;
    let mut hanzi = false;

    while bits.remaining() >= 4 {
        let mode = bits.take(4)?;
        match mode {
            0b0000 => break, // terminator
            0b0011 => {
                // structured append: 4-bit position · 4-bit total · 8-bit
                // parity — framing, not payload
                bits.take(16)?;
                structured_append = true;
            }
            0b0101 => {
                // FNC1 first position (§7.4.8.2) — after a second-position
                // declaration it describes no symbol ISO defines
                if fnc1 == Fnc1::Second && rules == Rules::Iso {
                    return None;
                }
                fnc1 = Fnc1::First;
            }
            0b1001 => {
                // FNC1 second position (§7.4.8.3): once, before all data
                if !out.is_empty() || (fnc1 != Fnc1::None && rules == Rules::Iso) {
                    return None;
                }
                application_indicator(&mut bits, &mut out)?;
                fnc1 = Fnc1::Second;
            }
            0b0111 => {
                let value = eci_designator(&mut bits, rules)?;
                eci.push(EciMark {
                    value,
                    start: out.len(),
                });
            }
            0b0001 => {
                let count = bits.take(count_bits(version, mode)?)? as usize;
                numeric(&mut bits, count, &mut out)?;
            }
            0b0010 => {
                let count = bits.take(count_bits(version, mode)?)? as usize;
                let chars = alphanumeric(&mut bits, count)?;
                if fnc1 == Fnc1::None {
                    out.extend_from_slice(&chars);
                } else {
                    push_fnc1_alphanumeric(&mut out, &chars);
                }
            }
            0b0100 => {
                let count = bits.take(count_bits(version, mode)?)? as usize;
                for _ in 0..count {
                    #[expect(clippy::cast_possible_truncation, reason = "take(8) ≤ 255")]
                    out.push(bits.take(8)? as u8);
                }
            }
            0b1000 | 0b1101 => {
                double_byte(&mut bits, mode, version, &mut out)?;
                hanzi |= mode == 0b1101;
            }
            _ => return None, // mode indicators ISO 18004 does not assign
        }
    }
    Some(Parsed {
        bytes: out,
        eci,
        fnc1,
        structured_append,
        hanzi,
    })
}

/// The AIM application indicator of FNC1 in second position (§7.4.8.3),
/// transmitted FIRST: two digits ("00"-"99") or one letter (value = ASCII
/// + 100). Anything else is no indicator.
fn application_indicator(bits: &mut Bits<'_>, out: &mut Vec<u8>) -> Option<()> {
    let indicator = bits.take(8)?;
    #[expect(clippy::cast_possible_truncation, reason = "take(8) ≤ 255")]
    match indicator {
        0..=99 => out.extend_from_slice(format!("{indicator:02}").as_bytes()),
        165..=190 | 197..=222 => out.push((indicator - 100) as u8),
        _ => return None,
    }
    Some(())
}

/// Numeric mode (§7.4.3): groups of three digits in 10 bits, a final pair
/// in 7, a final digit in 4 — each group within its decimal range.
fn numeric(bits: &mut Bits<'_>, count: usize, out: &mut Vec<u8>) -> Option<()> {
    let mut left = count;
    while left >= 3 {
        let v = bits.take(10)?;
        if v >= 1000 {
            return None;
        }
        out.extend_from_slice(format!("{v:03}").as_bytes());
        left -= 3;
    }
    if left == 2 {
        let v = bits.take(7)?;
        if v >= 100 {
            return None;
        }
        out.extend_from_slice(format!("{v:02}").as_bytes());
    } else if left == 1 {
        let v = bits.take(4)?;
        if v >= 10 {
            return None;
        }
        out.extend_from_slice(format!("{v}").as_bytes());
    }
    Some(())
}

/// Alphanumeric mode (§7.4.4): pairs in 11 bits, a final character in 6 —
/// the table characters, before any FNC1 `%` transmission rule.
fn alphanumeric(bits: &mut Bits<'_>, count: usize) -> Option<Vec<u8>> {
    let mut chars = Vec::with_capacity(count);
    let mut left = count;
    while left >= 2 {
        let v = bits.take(11)? as usize;
        let (a, b) = (v / 45, v % 45);
        if a >= 45 {
            return None;
        }
        chars.push(ALPHANUMERIC[a]);
        chars.push(ALPHANUMERIC[b]);
        left -= 2;
    }
    if left == 1 {
        let v = bits.take(6)? as usize;
        if v >= 45 {
            return None;
        }
        chars.push(ALPHANUMERIC[v]);
    }
    Some(chars)
}

/// Kanji (`0b1000`, §7.4.6) or Hanzi (`0b1101`, GB/T 18284) mode: each
/// 13-bit value is a two-byte pair `(v / split) << 8 | v % split`, plus
/// the low offset below `boundary` and the high one above it.
fn double_byte(bits: &mut Bits<'_>, mode: u32, version: u8, out: &mut Vec<u8>) -> Option<()> {
    let (split, boundary, low, high) = if mode == 0b1000 {
        // Shift JIS 8140-9FFC · E040-EBBF
        (0xC0, 0x1F00, 0x8140, 0xC140)
    } else {
        // GB 2312 subset (1) only: A1A1-AAFE · B0A1-FAFE
        if bits.take(4)? != 1 {
            return None;
        }
        (0x60, 0x0A00, 0xA1A1, 0xA6A1)
    };
    let count = bits.take(count_bits(version, mode)?)? as usize;
    for _ in 0..count {
        let v = bits.take(13)?;
        let assembled = (v / split) << 8 | (v % split);
        let pair = assembled + if assembled < boundary { low } else { high };
        #[expect(clippy::cast_possible_truncation, reason = "two 8-bit halves")]
        {
            out.push((pair >> 8) as u8);
            out.push((pair & 0xFF) as u8);
        }
    }
    Some(())
}

/// ECI assignment number per ISO 18004 Table 4: the leading 1-bits of the
/// first codeword count the extra codewords (`0bbbbbbb` · `10b{14}` ·
/// `110b{21}`); the prefix bits are NOT part of the value. `111xxxxx` has
/// no meaning and the register stops at 999999 — both refuse (the port
/// reads the latter as "unknown").
fn eci_designator(bits: &mut Bits<'_>, rules: Rules) -> Option<u32> {
    let first = bits.take(8)?;
    let value = if first & 0x80 == 0 {
        first
    } else if first & 0xC0 == 0x80 {
        (first & 0x3F) << 8 | bits.take(8)?
    } else if first & 0xE0 == 0xC0 {
        (first & 0x1F) << 16 | bits.take(16)?
    } else {
        return None;
    };
    (value <= 999_999 || rules == Rules::Port).then_some(value)
}

/// FNC1-mode alphanumeric data (ISO 18004 §7.4.8.2 · §7.4.8.3): a lone `%`
/// transmits the GS separator, `%%` transmits one literal `%` — resolved
/// left to right within the segment, as encoders double each data `%`.
fn push_fnc1_alphanumeric(out: &mut Vec<u8>, chars: &[u8]) {
    let mut rest = chars;
    while let Some((&c, tail)) = rest.split_first() {
        rest = tail;
        if c == b'%' {
            if let Some((b'%', after)) = rest.split_first() {
                out.push(b'%');
                rest = after;
            } else {
                out.push(GS);
            }
        } else {
            out.push(c);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Hand-assembled streams (MSB-first) — independent of any encoder.
    fn stream(bits: &str) -> Vec<u8> {
        let clean: String = bits.chars().filter(|c| !c.is_whitespace()).collect();
        clean
            .as_bytes()
            .chunks(8)
            .map(|chunk| {
                let mut byte = 0u8;
                for (i, &b) in chunk.iter().enumerate() {
                    if b == b'1' {
                        byte |= 1 << (7 - i);
                    }
                }
                byte
            })
            .collect()
    }

    fn marks(parsed: &Parsed) -> Vec<(u32, usize)> {
        parsed.eci.iter().map(|m| (m.value, m.start)).collect()
    }

    /// One 13-bit double-byte value as a bit string.
    fn thirteen(value: u32) -> String {
        (0..13)
            .rev()
            .map(|i| if value >> i & 1 == 1 { '1' } else { '0' })
            .collect()
    }

    #[test]
    fn numeric_groups_of_three_two_one() {
        // mode 0001 · count 5 (10 bits) · 012 (12) · 34 (34) → "01234"
        let parsed = parse(&stream("0001 0000000101 0000001100 0100010 0000"), 1).unwrap();
        assert_eq!(parsed.bytes, b"01234");
        assert_eq!(parsed.fnc1, Fnc1::None);
        assert!(parsed.eci.is_empty());
        assert!(!parsed.structured_append && !parsed.hanzi);
    }

    #[test]
    fn alphanumeric_pair_and_remainder() {
        // "AC-" : A=10,C=12 → 10*45+12=462 · '-'=41
        // mode 0010 · count 3 (9 bits) · 462 (11 bits) · 41 (6 bits)
        let parsed = parse(&stream("0010 000000011 00111001110 101001 0000"), 1).unwrap();
        assert_eq!(parsed.bytes, b"AC-");
    }

    #[test]
    fn byte_mode_passthrough() {
        // mode 0100 · count 3 (8 bits) · "qr!"
        let data = stream("0100 00000011 01110001 01110010 00100001 0000");
        assert_eq!(parse(&data, 1).unwrap().bytes, b"qr!");
    }

    #[test]
    fn kanji_roundtrip_against_shift_jis() {
        // 日 = SJIS 0x93FA: the forward ISO mapping builds the 13-bit
        // value; the parser must invert it.
        let adj: u16 = 0x93FA - 0x8140;
        let value = u32::from(adj >> 8) * 0xC0 + u32::from(adj & 0xFF);
        let data = stream(&format!("1000 00000001 {} 0000", thirteen(value)));
        let parsed = parse(&data, 1).unwrap();
        assert_eq!(parsed.bytes, vec![0x93, 0xFA]);
        assert!(!parsed.hanzi);
    }

    #[test]
    fn kanji_upper_block_boundary_at_0x1f00() {
        // assembled == 0x1F00 is the FIRST value of the upper SJIS block
        // (ISO §7.4.6): `< 0x1F00` is FALSE → + 0xC140 → 0xE040. A `<`→`<=`
        // swap adds 0x8140 → 0xA040; `+`→`-`/`*` corrupt the offset.
        // v packing 0x1F00 = 31*0xC0 + 0 = 5952.
        let data = stream(&format!("1000 00000001 {} 0000", thirteen(5952)));
        assert_eq!(parse(&data, 1).unwrap().bytes, vec![0xE0, 0x40]);
    }

    #[test]
    fn hanzi_gb2312_pairs() {
        // GB/T 18284: 0xB0A1 ('啊') sits past the 0x0A00 split: minus
        // 0xA6A1 = 0x0A00 → 0x0A * 0x60 + 0x00 = 960. 0xA1A1 is the first
        // pair of the low block: 0.
        let data = stream(&format!(
            "1101 0001 00000010 {} {} 0000",
            thirteen(960),
            thirteen(0)
        ));
        let parsed = parse(&data, 1).unwrap();
        assert_eq!(parsed.bytes, vec![0xB0, 0xA1, 0xA1, 0xA1]);
        assert!(parsed.hanzi);
        // only the GB 2312 subset (1) is defined
        let other = stream(&format!("1101 0010 00000001 {} 0000", thirteen(960)));
        assert!(parse(&other, 1).is_none());
    }

    #[test]
    fn fnc1_first_lone_percent_is_gs() {
        // FNC1(0101) · alpha count 1 · '%' (=38)
        let parsed = parse(&stream("0101 0010 000000001 100110 0000"), 1).unwrap();
        assert_eq!(parsed.fnc1, Fnc1::First);
        assert_eq!(parsed.bytes, vec![GS]);
    }

    #[test]
    fn fnc1_first_double_percent_is_one_literal_percent() {
        // ISO 18004 §7.4.8.2 example 2: "123%" encodes as "123%%" and
        // transmits "123%". Here FNC1 · alpha "B%%C" (count 4):
        // B=11,%=38 → 533 · %=38,C=12 → 1722.
        let data = stream("0101 0010 000000100 01000010101 11010111010 0000");
        let parsed = parse(&data, 1).unwrap();
        assert_eq!(parsed.bytes, b"B%C", "%% transmits one %, never two GS");
        // left to right: "%%%" is one literal % then one separator
        let mut out = Vec::new();
        push_fnc1_alphanumeric(&mut out, b"%%%A%");
        assert_eq!(out, vec![b'%', GS, b'A', GS]);
    }

    #[test]
    fn fnc1_does_not_gs_non_percent_alpha() {
        // FNC1 active + a NON-'%' alpha char is emitted verbatim.
        let parsed = parse(&stream("0101 0010 000000001 001010 0000"), 1).unwrap();
        assert_eq!(parsed.bytes, b"A");
    }

    #[test]
    fn percent_without_fnc1_stays_literal() {
        let parsed = parse(&stream("0010 000000001 100110 0000"), 1).unwrap();
        assert_eq!(parsed.fnc1, Fnc1::None);
        assert_eq!(parsed.bytes, b"%");
    }

    #[test]
    fn fnc1_second_transmits_the_application_indicator_first() {
        // mode 1001 · indicator 37 · byte "abc" → "37abc": an AIM
        // application, not GS1.
        let data = stream("1001 00100101 0100 00000011 01100001 01100010 01100011 0000");
        let parsed = parse(&data, 1).unwrap();
        assert_eq!(parsed.bytes, b"37abc");
        assert_eq!(parsed.fnc1, Fnc1::Second);
        // one-digit numbers keep two digits: 5 → "05"
        assert_eq!(
            parse(&stream("1001 00000101 0000"), 1).unwrap().bytes,
            b"05"
        );
        // a letter is its ASCII value + 100: 'A' = 165, 'z' = 222
        assert_eq!(parse(&stream("1001 10100101 0000"), 1).unwrap().bytes, b"A");
        assert_eq!(parse(&stream("1001 11011110 0000"), 1).unwrap().bytes, b"z");
    }

    #[test]
    fn fnc1_second_alphanumeric_percent_rules_apply_too() {
        // §7.4.8.3 repeats the % / %% transmission rule for the second
        // position: indicator 37 · alpha "%A" (count 2): 38*45+10 = 1720.
        let data = stream("1001 00100101 0010 000000010 11010111000 0000");
        assert_eq!(parse(&data, 1).unwrap().bytes, vec![b'3', b'7', GS, b'A']);
    }

    #[test]
    fn fnc1_second_refuses_invalid_or_late_indicators() {
        // 100..=164 is neither two digits nor a letter
        assert!(parse(&stream("1001 01100100 0000"), 1).is_none());
        // after data: the indicator must precede every data segment
        let late = stream("0100 00000001 01100001 1001 00100101 0000");
        assert!(parse(&late, 1).is_none());
        // first + second, either order: no symbol ISO defines
        assert!(parse(&stream("0101 1001 00100101 0000"), 1).is_none());
        assert!(parse(&stream("1001 00100101 0101 0000"), 1).is_none());
    }

    #[test]
    fn port_payload_names_what_the_port_transmits_for_a_refused_stream() {
        let abc = "0100 00000011 01100001 01100010 01100011 0000";
        // above the register (1000000): refused, the port reads "abc"
        let above = stream(&format!("0111 11001111 01000010 01000000 {abc}"));
        assert!(parse(&above, 1).is_none());
        assert_eq!(port_payload(&above, 1).as_deref(), Some(&b"abc"[..]));
        // FNC1 in both positions, either order: the indicator leads
        for order in ["1001 00100101 0101", "0101 1001 00100101"] {
            let both = stream(&format!("{order} {abc}"));
            assert!(parse(&both, 1).is_none(), "{order}");
            assert_eq!(
                port_payload(&both, 1).as_deref(),
                Some(&b"37abc"[..]),
                "{order}"
            );
        }
        // what the port refuses too stays refused: a 111xxxxx designator,
        // a late, repeated or invalid application indicator
        for refused in [
            format!("0111 11100000 {abc}"),
            format!("0100 00000001 01100001 1001 00100101 {abc}"),
            format!("1001 00100101 1001 00100101 {abc}"),
            "1001 01100100 0000".to_owned(),
        ] {
            assert!(port_payload(&stream(&refused), 1).is_none(), "{refused}");
        }
        // a well-formed stream transmits its own payload
        let declared = stream("0111 00000100 0100 00000001 10100001 0000");
        assert_eq!(
            port_payload(&declared, 1),
            parse(&declared, 1).map(|p| p.bytes)
        );
    }

    #[test]
    fn eci_one_byte_designator_records_value_and_offset() {
        // 1-byte ECI (first = 5, high bit CLEAR) → NO extra byte, then 'U'.
        // An `&`→`^`/`|` or `==`→`!=` swap would enter a multi-byte branch,
        // swallowing the next segment header → misparse.
        let data = stream("0111 00000101 0100 00000001 01010101 0000");
        let parsed = parse(&data, 1).unwrap();
        assert_eq!(parsed.bytes, vec![0x55]);
        assert_eq!(marks(&parsed), vec![(5, 0)]);
    }

    #[test]
    fn eci_two_byte_designator_masks_its_prefix() {
        // 2-byte ECI 10|000000 00000010 = 2: exactly ONE extra byte; the
        // prefix bits are not part of the value (Table 4).
        let data = stream("0111 10000000 00000010 0100 00000001 01011010 0000");
        let parsed = parse(&data, 1).unwrap();
        assert_eq!(parsed.bytes, vec![0x5A]);
        assert_eq!(marks(&parsed), vec![(2, 0)]);
    }

    #[test]
    fn eci_three_byte_designator_masks_its_prefix() {
        // 3-byte ECI 110|00000 00000011 00000100 = 0x0304 = 772: exactly
        // TWO extra bytes.
        let data = stream("0111 11000000 00000011 00000100 0100 00000001 01111110 0000");
        let parsed = parse(&data, 1).unwrap();
        assert_eq!(parsed.bytes, vec![0x7E]);
        assert_eq!(marks(&parsed), vec![(772, 0)]);
    }

    #[test]
    fn eci_marks_carry_the_payload_offset_where_they_take_effect() {
        // numeric "12" · ECI 4 · byte A1 · ECI 26 · byte 41
        let data = stream(
            "0001 0000000010 0001100 \
             0111 00000100 0100 00000001 10100001 \
             0111 00011010 0100 00000001 01000001 0000",
        );
        let parsed = parse(&data, 1).unwrap();
        assert_eq!(parsed.bytes, vec![b'1', b'2', 0xA1, 0x41]);
        assert_eq!(marks(&parsed), vec![(4, 2), (26, 3)]);
    }

    #[test]
    fn malformed_eci_designators_refuse() {
        // 111xxxxx has no Table 4 form — rqrr reads it as one byte, rxing
        // and zbar reject it; the scanner refuses (never a guessed value)
        let data = stream("0111 11100000 0100 00000001 01000001 0000");
        assert!(parse(&data, 1).is_none());
        assert!(parse(&stream("0111 11111111 0000"), 1).is_none());
        // 110 form above 999999 (0x1FFFFF): outside the register
        let data = stream("0111 11011111 11111111 11111111 0000");
        assert!(parse(&data, 1).is_none());
        // 999999 itself is the last assignable number: 0x0F423F
        let data = stream("0111 11001111 01000010 00111111 0000");
        assert_eq!(marks(&parse(&data, 1).unwrap()), vec![(999_999, 0)]);
        // truncated designators refuse
        assert!(parse(&stream("0111 1000"), 1).is_none());
    }

    #[test]
    fn structured_append_header_is_framing_not_payload() {
        // 0011 · position 0 · total-1 = 1 · parity 0x41 · byte "A"
        let data = stream("0011 0000 0001 01000001 0100 00000001 01000001 0000");
        let parsed = parse(&data, 1).unwrap();
        assert!(parsed.structured_append);
        assert_eq!(parsed.bytes, b"A");
    }

    #[test]
    fn numeric_single_leftover_digit() {
        // count 1 → the `left == 1` tail handles ONE digit (4 bits);
        // `>=`→`<` on its range check would reject this valid 7.
        let parsed = parse(&stream("0001 0000000001 0111 0000"), 1).unwrap();
        assert_eq!(parsed.bytes, b"7");
    }

    #[test]
    fn numeric_single_digit_ten_is_malformed() {
        // a 4-bit leftover ≥ 10 is not a legal digit → refuse
        assert!(parse(&stream("0001 0000000001 1010 0000"), 1).is_none());
    }

    #[test]
    fn alphanumeric_even_pair_leaves_no_remainder() {
        // count 2 (EVEN) → ONE pair, no tail. "AB": 10*45 + 11 = 461.
        let parsed = parse(&stream("0010 000000010 00111001101 0000"), 1).unwrap();
        assert_eq!(parsed.bytes, b"AB");
    }

    proptest::proptest! {
        /// Total over attacker-shaped codewords: refuse or parse, never
        /// panic; every recorded offset lies inside the returned payload,
        /// in stream order; the interpretation of what parsed is total; and
        /// the port's tolerance only ever adds payloads (a well-formed
        /// stream transmits the same bytes through it).
        #[test]
        fn parse_is_total_and_its_offsets_stay_in_bounds(
            data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..48),
            version in 0u8..42,
        ) {
            let tolerant = port_payload(&data, version);
            if let Some(parsed) = parse(&data, version) {
                proptest::prop_assert_eq!(tolerant.as_deref(), Some(parsed.bytes.as_slice()));
                let mut previous = 0;
                for mark in &parsed.eci {
                    proptest::prop_assert!(mark.value <= 999_999);
                    proptest::prop_assert!(previous <= mark.start);
                    proptest::prop_assert!(mark.start <= parsed.bytes.len());
                    previous = mark.start;
                }
                let evidence = crate::engine::content::EciEvidence::from_marks(parsed.eci);
                let reading = crate::engine::content::read(&parsed.bytes, &evidence);
                proptest::prop_assert!(parsed.bytes.is_empty() || !reading.text.is_empty());
            }
        }
    }

    #[test]
    fn malformed_streams_refuse() {
        // numeric triple ≥ 1000 is not a legal encode
        assert!(parse(&stream("0001 0000000011 1111101000"), 1).is_none());
        // truncated count field
        assert!(parse(&stream("0100 0000"), 1).is_none());
        // unassigned mode indicators
        assert!(parse(&stream("1110 00000000"), 1).is_none());
        assert!(parse(&stream("0110 00000000"), 1).is_none());
        // a version outside 1..=40 has no count widths
        assert!(parse(&stream("0100 00000001 01000001"), 0).is_none());
        // empty stream is a valid empty payload (terminator-only semantics)
        assert_eq!(parse(&[], 1).unwrap().bytes, Vec::<u8>::new());
    }
}
