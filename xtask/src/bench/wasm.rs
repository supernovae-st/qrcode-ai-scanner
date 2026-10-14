//! The build recipe of a WASM module: its `producers` custom section
//! (toolchain, wasm-bindgen, post-processors), its `target_features`
//! section when present, and whether its code uses SIMD. A base / candidate
//! pair built by different recipes is refused — a plain `wasm-pack` build
//! against the release script's `+simd128` and `wasm-opt -O3` would differ
//! for build reasons alone.
//!
//! SIMD is read from the code itself: every function body is decoded
//! instruction by instruction (so immediates are never mistaken for
//! opcodes) until a `0xFD`-prefixed instruction appears. An opcode this
//! decoder does not know leaves the answer unknown, never guessed.

use std::collections::BTreeMap;

use serde::Serialize;

/// The recipe facts of one module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Recipe {
    /// `producers` fields (`language`, `processed-by`, `sdk`) → `name version`.
    pub(crate) producers: BTreeMap<String, Vec<String>>,
    pub(crate) wasm_bindgen: Option<String>,
    /// `+feature` / `-feature` entries of a `target_features` section.
    pub(crate) target_features: Option<Vec<String>>,
    /// `None` when the code could not be decoded.
    pub(crate) simd: Option<bool>,
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Option<u8> {
        let b = *self.data.get(self.at)?;
        self.at += 1;
        Some(b)
    }

    fn uleb(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            value |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    /// A signed LEB: only its length matters here.
    fn sleb(&mut self) -> Option<()> {
        for _ in 0..10 {
            if self.byte()? & 0x80 == 0 {
                return Some(());
            }
        }
        None
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let slice = self.data.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(slice)
    }

    fn name(&mut self) -> Option<String> {
        let len = usize::try_from(self.uleb()?).ok()?;
        Some(String::from_utf8_lossy(self.bytes(len)?).into_owned())
    }

    fn done(&self) -> bool {
        self.at >= self.data.len()
    }
}

fn producers(body: &[u8]) -> Option<BTreeMap<String, Vec<String>>> {
    let mut r = Reader { data: body, at: 0 };
    let mut out = BTreeMap::new();
    for _ in 0..r.uleb()? {
        let field = r.name()?;
        let mut values = Vec::new();
        for _ in 0..r.uleb()? {
            let name = r.name()?;
            let version = r.name()?;
            values.push(format!("{name} {version}"));
        }
        out.insert(field, values);
    }
    Some(out)
}

fn target_features(body: &[u8]) -> Option<Vec<String>> {
    let mut r = Reader { data: body, at: 0 };
    let mut out = Vec::new();
    for _ in 0..r.uleb()? {
        let prefix = char::from(r.byte()?);
        out.push(format!("{prefix}{}", r.name()?));
    }
    Some(out)
}

/// A block type: empty, a value type, or a type index (s33).
fn block_type(r: &mut Reader<'_>) -> Option<()> {
    let first = *r.data.get(r.at)?;
    if first == 0x40 || matches!(first, 0x6f | 0x70 | 0x7b..=0x7f) {
        r.at += 1;
        Some(())
    } else {
        r.sleb()
    }
}

/// A memory argument: alignment (bit 6 announces a memory index), offset.
fn memarg(r: &mut Reader<'_>) -> Option<()> {
    let align = r.uleb()?;
    if align & 0x40 != 0 {
        r.uleb()?;
    }
    r.uleb().map(|_| ())
}

/// Whether one function body uses a `0xFD` (SIMD) instruction; `None` on
/// an opcode this decoder does not know.
fn body_uses_simd(body: &[u8]) -> Option<bool> {
    let mut r = Reader { data: body, at: 0 };
    for _ in 0..r.uleb()? {
        r.uleb()?;
        let ty = r.byte()?;
        if ty == 0x7b {
            return Some(true);
        }
    }
    while !r.done() {
        let op = r.byte()?;
        match op {
            0x00
            | 0x01
            | 0x05
            | 0x0a
            | 0x0b
            | 0x0f
            | 0x19
            | 0x1a
            | 0x1b
            | 0x45..=0xc4
            | 0xd1
            | 0xd3
            | 0xd5 => {}
            0x02..=0x04 | 0x06 => block_type(&mut r)?,
            // one index immediate (memory.size / memory.grow: a memory index)
            0x07..=0x09
            | 0x0c
            | 0x0d
            | 0x10
            | 0x12
            | 0x14
            | 0x15
            | 0x18
            | 0x20..=0x26
            | 0x3f
            | 0x40
            | 0xd2
            | 0xd4
            | 0xd6 => {
                r.uleb()?;
            }
            0x0e => {
                for _ in 0..r.uleb()? {
                    r.uleb()?;
                }
                r.uleb()?;
            }
            0x11 | 0x13 => {
                r.uleb()?;
                r.uleb()?;
            }
            0x1c => {
                let n = usize::try_from(r.uleb()?).ok()?;
                r.bytes(n)?;
            }
            0x28..=0x3e => memarg(&mut r)?,
            0x41 | 0x42 => r.sleb()?,
            0x43 => {
                r.bytes(4)?;
            }
            0x44 => {
                r.bytes(8)?;
            }
            0xd0 => {
                r.byte()?;
            }
            0xfc => match r.uleb()? {
                0..=7 => {}
                9 | 11 | 13 | 15..=17 => {
                    r.uleb()?;
                }
                8 | 10 | 12 | 14 => {
                    r.uleb()?;
                    r.uleb()?;
                }
                _ => return None,
            },
            0xfd => return Some(true),
            0xfe => {
                if r.uleb()? == 0x03 {
                    r.byte()?;
                } else {
                    memarg(&mut r)?;
                }
            }
            _ => return None,
        }
    }
    Some(false)
}

fn code_uses_simd(section: &[u8]) -> Option<bool> {
    let mut r = Reader {
        data: section,
        at: 0,
    };
    for _ in 0..r.uleb()? {
        let size = usize::try_from(r.uleb()?).ok()?;
        if body_uses_simd(r.bytes(size)?)? {
            return Some(true);
        }
    }
    Some(false)
}

/// Read the recipe facts of a `.wasm` module.
pub(crate) fn recipe(module: &[u8]) -> Result<Recipe, String> {
    if module.get(..4) != Some(b"\0asm".as_slice()) {
        return Err(String::from("not a wasm module"));
    }
    let mut r = Reader {
        data: module,
        at: 8,
    };
    let mut found = Recipe {
        producers: BTreeMap::new(),
        wasm_bindgen: None,
        target_features: None,
        simd: Some(false),
    };
    let mut code_seen = false;
    while !r.done() {
        let id = r.byte().ok_or("truncated section header")?;
        let size = r
            .uleb()
            .and_then(|s| usize::try_from(s).ok())
            .ok_or("bad section size")?;
        let body = r.bytes(size).ok_or("truncated section")?;
        match id {
            0 => {
                let mut inner = Reader { data: body, at: 0 };
                let name = inner.name().ok_or("bad custom section name")?;
                let rest = body.get(inner.at..).unwrap_or_default();
                match name.as_str() {
                    "producers" => {
                        found.producers = producers(rest).ok_or("bad producers section")?;
                    }
                    "target_features" => found.target_features = target_features(rest),
                    _ => {}
                }
            }
            10 => {
                code_seen = true;
                found.simd = code_uses_simd(body);
            }
            _ => {}
        }
    }
    if !code_seen {
        found.simd = Some(false);
    }
    found.wasm_bindgen = found
        .producers
        .get("processed-by")
        .and_then(|tools| tools.iter().find_map(|t| t.strip_prefix("wasm-bindgen ")))
        .map(str::to_owned);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(id: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![id, u8::try_from(body.len()).expect("small section")];
        out.extend_from_slice(body);
        out
    }

    fn named(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut body = vec![u8::try_from(name.len()).expect("short name")];
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(payload);
        section(0, &body)
    }

    fn module(code: &[u8], extra: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"\0asm\x01\0\0\0".to_vec();
        // (type (func)), (func (type 0)), then the code
        out.extend(section(1, &[1, 0x60, 0, 0]));
        out.extend(section(3, &[1, 0]));
        let mut body = vec![0u8]; // no locals
        body.extend_from_slice(code);
        let mut code_section = vec![1, u8::try_from(body.len()).expect("small body")];
        code_section.extend(body);
        out.extend(section(10, &code_section));
        for s in extra {
            out.extend_from_slice(s);
        }
        out
    }

    /// `i32.const 0x7ffd` carries an 0xFD byte in its immediate — the
    /// decoder must not read it as a SIMD prefix. `v128.const` is SIMD.
    #[test]
    fn simd_is_read_from_decoded_instructions() {
        let scalar = module(&[0x41, 0xfd, 0xff, 0x01, 0x1a, 0x0b], &[]);
        assert_eq!(recipe(&scalar).expect("module").simd, Some(false));
        let mut v128 = vec![0xfd, 0x0c];
        v128.extend([0u8; 16]);
        v128.extend([0x1a, 0x0b]);
        assert_eq!(
            recipe(&module(&v128, &[])).expect("module").simd,
            Some(true)
        );
        let unknown = module(&[0xff, 0x0b], &[]);
        assert_eq!(
            recipe(&unknown).expect("module").simd,
            None,
            "unknown opcode"
        );
        let memory = module(&[0x41, 0x00, 0x28, 0x02, 0x08, 0x1a, 0x0b], &[]);
        assert_eq!(
            recipe(&memory).expect("module").simd,
            Some(false),
            "i32.load memarg"
        );
        assert!(recipe(b"not wasm").is_err());
    }

    #[test]
    fn producers_and_target_features_are_decoded() {
        let mut producers = vec![2u8];
        for (field, values) in [
            ("language", vec![("Rust", "")]),
            (
                "processed-by",
                vec![("rustc", "1.97.0"), ("wasm-bindgen", "0.2.126")],
            ),
        ] {
            producers.push(u8::try_from(field.len()).expect("short"));
            producers.extend_from_slice(field.as_bytes());
            producers.push(u8::try_from(values.len()).expect("few"));
            for (name, version) in values {
                producers.push(u8::try_from(name.len()).expect("short"));
                producers.extend_from_slice(name.as_bytes());
                producers.push(u8::try_from(version.len()).expect("short"));
                producers.extend_from_slice(version.as_bytes());
            }
        }
        let features = [1u8, b'+', 7, b's', b'i', b'm', b'd', b'1', b'2', b'8'];
        let wasm = module(
            &[0x0b],
            &[
                named("producers", &producers),
                named("target_features", &features),
            ],
        );
        let found = recipe(&wasm).expect("module");
        assert_eq!(found.wasm_bindgen.as_deref(), Some("0.2.126"));
        assert_eq!(
            found.producers.get("processed-by"),
            Some(&vec![
                String::from("rustc 1.97.0"),
                String::from("wasm-bindgen 0.2.126")
            ])
        );
        assert_eq!(found.target_features, Some(vec![String::from("+simd128")]));
        assert_eq!(found.simd, Some(false));
    }
}
