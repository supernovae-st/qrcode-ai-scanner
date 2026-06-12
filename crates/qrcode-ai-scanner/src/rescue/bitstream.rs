//! Corrected data codewords → payload bytes (ISO/IEC 18004 §7.4) — the
//! decode half the engines normally own, needed because a rescued stream
//! never reaches them. Modes: numeric · alphanumeric · byte · kanji · ECI
//! (consumed, value dropped — `charset::resolve` sniffs downstream, same
//! posture as rqrr) · FNC1 (flagged). Differentially pinned against the
//! engines on every clean corpus fixture.

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

/// Character-count field width per mode class and version (ISO table 3).
fn count_bits(version: u8, mode: u32) -> Option<usize> {
    let class = match version {
        1..=9 => 0,
        10..=26 => 1,
        27..=40 => 2,
        _ => return None,
    };
    Some(match mode {
        0b0001 => [10, 12, 14][class], // numeric
        0b0010 => [9, 11, 13][class],  // alphanumeric
        0b0100 => [8, 16, 16][class],  // byte
        0b1000 => [8, 10, 12][class],  // kanji
        _ => return None,
    })
}

const ALPHANUMERIC: &[u8; 45] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:";

/// Parse the data segment stream. Returns the payload bytes + whether the
/// symbol declared FNC1-in-first-position. `None` on any malformed
/// structure (a rescue that corrected to garbage must not surface).
pub(super) fn parse(data: &[u8], version: u8) -> Option<(Vec<u8>, bool)> {
    let mut bits = Bits {
        bytes: data,
        cursor: 0,
    };
    let mut out = Vec::new();
    let mut fnc1 = false;

    while bits.remaining() >= 4 {
        let mode = bits.take(4)?;
        match mode {
            0b0000 => break, // terminator
            0b0101 => fnc1 = true,
            0b1001 => {
                // FNC1 second position: 8-bit application indicator
                bits.take(8)?;
                fnc1 = true;
            }
            0b0111 => {
                // ECI: 1/2/3-byte designator (value consumed, charset is
                // resolved downstream over the raw bytes)
                let first = bits.take(8)?;
                if first & 0x80 != 0 {
                    if first & 0x40 == 0 {
                        bits.take(8)?;
                    } else {
                        bits.take(16)?;
                    }
                }
            }
            0b0001 => {
                let count = bits.take(count_bits(version, mode)?)? as usize;
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
            }
            0b0010 => {
                let count = bits.take(count_bits(version, mode)?)? as usize;
                let mut left = count;
                while left >= 2 {
                    let v = bits.take(11)? as usize;
                    let (a, b) = (v / 45, v % 45);
                    if a >= 45 {
                        return None;
                    }
                    push_alpha(&mut out, a, fnc1);
                    push_alpha(&mut out, b, fnc1);
                    left -= 2;
                }
                if left == 1 {
                    let v = bits.take(6)? as usize;
                    if v >= 45 {
                        return None;
                    }
                    push_alpha(&mut out, v, fnc1);
                }
            }
            0b0100 => {
                let count = bits.take(count_bits(version, mode)?)? as usize;
                for _ in 0..count {
                    #[expect(clippy::cast_possible_truncation, reason = "take(8) ≤ 255")]
                    out.push(bits.take(8)? as u8);
                }
            }
            0b1000 => {
                let count = bits.take(count_bits(version, mode)?)? as usize;
                for _ in 0..count {
                    let v = bits.take(13)?;
                    // ISO §7.4.6 inverse: 13-bit value → Shift-JIS pair
                    let assembled = (v / 0xC0) << 8 | (v % 0xC0);
                    let sjis = if assembled < 0x1F00 {
                        assembled + 0x8140
                    } else {
                        assembled + 0xC140
                    };
                    #[expect(clippy::cast_possible_truncation, reason = "two 8-bit halves")]
                    {
                        out.push((sjis >> 8) as u8);
                        out.push((sjis & 0xFF) as u8);
                    }
                }
            }
            _ => return None, // structured append (0011) and unknowns: refuse
        }
    }
    Some((out, fnc1))
}

/// Alphanumeric table char; in FNC1 mode `%` is the GS escape (ISO §7.4.8.3
/// — `%%` literal handling lives at pair granularity upstream in spirit;
/// the engines emit GS for single `%`, matched here).
fn push_alpha(out: &mut Vec<u8>, index: usize, fnc1: bool) {
    let c = ALPHANUMERIC[index];
    if fnc1 && c == b'%' {
        out.push(0x1D);
    } else {
        out.push(c);
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

    #[test]
    fn numeric_groups_of_three_two_one() {
        // mode 0001 · count 5 (10 bits) · 012 (12) · 34 (34) → "01234"
        let data = stream("0001 0000000101 0000001100 0100010 0000");
        let (raw, fnc1) = parse(&data, 1).unwrap();
        assert_eq!(raw, b"01234");
        assert!(!fnc1);
    }

    #[test]
    fn alphanumeric_pair_and_remainder() {
        // "AC-" : A=10,C=12 → 10*45+12=462 · '-'=41
        // mode 0010 · count 3 (9 bits) · 462 (11 bits) · 41 (6 bits)
        let data = stream("0010 000000011 00111001110 101001 0000");
        let (raw, _) = parse(&data, 1).unwrap();
        assert_eq!(raw, b"AC-");
    }

    #[test]
    fn byte_mode_passthrough() {
        // mode 0100 · count 3 (8 bits) · "qr!"
        let data = stream("0100 00000011 01110001 01110010 00100001 0000");
        let (raw, _) = parse(&data, 1).unwrap();
        assert_eq!(raw, b"qr!");
    }

    #[test]
    fn kanji_roundtrip_against_shift_jis() {
        // 日 = SJIS 0x93FA → subtract 0x8140 = 0x12BA → msb 0x12·0xC0+0xBA = 0x142A? — use
        // the forward ISO mapping to build the value, then assert the parser inverts it.
        let sjis: u16 = 0x93FA;
        let adj = sjis - 0x8140;
        let value = u32::from(adj >> 8) * 0xC0 + u32::from(adj & 0xFF);
        let mut bits_str = String::from("1000 00000001 ");
        for i in (0..13).rev() {
            bits_str.push(if value >> i & 1 == 1 { '1' } else { '0' });
        }
        bits_str.push_str(" 0000");
        let (raw, _) = parse(&stream(&bits_str), 1).unwrap();
        assert_eq!(raw, vec![0x93, 0xFA]);
    }

    #[test]
    fn fnc1_first_flags_and_percent_becomes_gs_in_alpha() {
        // FNC1(0101) · alpha count 1 · '%' (=38)
        let data = stream("0101 0010 000000001 100110 0000");
        let (raw, fnc1) = parse(&data, 1).unwrap();
        assert!(fnc1);
        assert_eq!(raw, vec![0x1D]);
    }

    #[test]
    fn malformed_streams_refuse() {
        // numeric triple ≥ 1000 is not a legal encode
        assert!(parse(&stream("0001 0000000011 1111101000"), 1).is_none());
        // truncated count field
        assert!(parse(&stream("0100 0000"), 1).is_none());
        // structured append refused
        assert!(parse(&stream("0011 00000000"), 1).is_none());
        // empty stream is a valid empty payload (terminator-only semantics)
        assert_eq!(parse(&[], 1).unwrap().0, Vec::<u8>::new());
    }
}
