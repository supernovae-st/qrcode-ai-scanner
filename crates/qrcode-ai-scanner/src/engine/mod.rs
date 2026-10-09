//! Decode engines — isolated wrappers around third-party decoders.
//!
//! Contract: engines NEVER panic outward (`catch_unwind` boundary), never
//! decide text (they report payload bytes + what they saw of the ECI
//! declarations; the interpretation happens once, in `content`), and only
//! report what they actually measured — no stubs.

pub(crate) mod charset;
pub(crate) mod content;
#[cfg(feature = "engine-rqrr")]
mod rqrr_engine;
#[cfg(feature = "engine-rxing")]
mod rxing_engine;

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::input::LumaImage;
use crate::report::{EcLevel, EngineKind, Point, Symbology};
use crate::rescue::bitstream::PortRead;
use content::{ContentKey, EciEvidence, Fnc1};

/// Still-masked codeword bitstream as the engine sampled it (rqrr only) —
/// feeds the synthetic UEC.
#[derive(Debug, Clone)]
pub(crate) struct MaskedStream {
    /// Packed bits, MSB-first per byte.
    pub bits: Vec<u8>,
    /// Stream length in bits.
    pub bit_len: usize,
}

/// What an engine actually measured — no stubs, `None` means "not provided".
#[derive(Debug, Clone)]
pub(crate) struct RawDetection {
    /// Symbology the engine read this as.
    pub symbology: Symbology,
    /// Payload bytes. QR family: the ORIGINAL bytes, ECI designators
    /// excluded, whichever engine read them — never engine text re-encoded.
    pub raw: Vec<u8>,
    /// What the observation saw of the ECI declarations governing `raw`
    /// (QR family; `Unobserved` elsewhere).
    pub eci: EciEvidence,
    /// Raw sampled bitstream when the engine exposes it (rqrr).
    pub masked_stream: Option<MaskedStream>,
    /// Symbol corners when the engine provides true bounds (rqrr).
    pub corners: Option<[Point; 4]>,
    /// Symbol version 1-40 when measured.
    pub version: Option<u8>,
    /// Error-correction level when measured.
    pub ec: Option<EcLevel>,
    /// Mask pattern 0-7 when measured.
    pub mask: Option<u8>,
    /// The FNC1 position the read observed. First = GS1 formatted data (QR
    /// `]Q3`/`]Q4` · `DataMatrix` `]d2` · GS1-128 `]C1`); second = an AIM
    /// application, never GS1. rqrr cannot decode FNC1 symbols at all (its
    /// mode dispatch rejects indicators 0x5/0x9); rxing and the rescue
    /// parser can.
    pub fnc1: Fnc1,
    /// The scanner's own segment parser read this stream (an rqrr decode's
    /// re-derivation, rxing's codeword output, the rescue): its ECI and
    /// FNC1 account is exposed, not stated by an engine. A stream proven
    /// malformed vetoes only reads that are not parsed — equal bytes from
    /// a parsed stream belong to another, well-formed symbol.
    pub parsed: bool,
    /// Producing engine.
    pub engine: EngineKind,
}

/// What a proven-malformed QR stream refuses — the reads of it that carry
/// no well-formed stream of their own (ISO 18004 defines no symbol for it).
/// A refusal is a content, never bytes alone: a well-formed symbol with
/// equal bytes is another content and keeps its reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Veto {
    /// The content rxing's zxing-cpp port reports for the stream — its
    /// bytes, a designator signalled iff the port read one, its FNC1
    /// position: refuses the unparsed QR reads that content admits.
    Port(ContentKey),
    /// The bytes rqrr decoded from the stream (its own conversion): refuses
    /// unparsed rqrr reads of them, nothing else.
    Rqrr(Vec<u8>),
}

impl Veto {
    /// The refusal of what rxing's port reports for a refused stream.
    pub(crate) fn port(read: PortRead) -> Self {
        Self::Port(ContentKey {
            raw: read.bytes,
            eci: if read.signalled {
                EciEvidence::Signalled
            } else {
                EciEvidence::NoEci
            },
            fnc1: read.fnc1,
        })
    }

    /// Whether this refusal covers one engine hit: an unparsed QR read of
    /// the refused content.
    pub(crate) fn refuses(&self, hit: &RawDetection) -> bool {
        hit.symbology == Symbology::QrCode
            && !hit.parsed
            && match self {
                Self::Port(content) => content.admits(hit),
                Self::Rqrr(bytes) => hit.engine == EngineKind::Rqrr && hit.raw == *bytes,
            }
    }

    /// Whether it covers a detection whose reads are all unparsed — of
    /// `engines`, reconciled into `key`.
    pub(crate) fn refuses_content(&self, key: &ContentKey, engines: &[EngineKind]) -> bool {
        match self {
            Self::Port(content) => content.agrees(key),
            Self::Rqrr(bytes) => {
                key.raw == *bytes && engines.iter().all(|&engine| engine == EngineKind::Rqrr)
            }
        }
    }
}

/// Aggregated result of one pass over all enabled engines.
#[derive(Debug, Clone, Default)]
pub(crate) struct EngineOutcome {
    /// Every detection from every engine (deduplication is the ladder's job).
    pub detections: Vec<RawDetection>,
    /// Grids detected but NOT decoded (rqrr `get_raw_data` ok, RS failed) —
    /// the S5 rescue inputs. Corners are in ATTEMPT space here.
    pub rescue: Vec<crate::rescue::RescueCandidate>,
    /// QR streams proven malformed in this pass, as what they refuse: the
    /// bytes rqrr decoded from a stream the scanner's parser refuses, and
    /// the content rxing's port reports for it. The ladder vetoes the reads
    /// these cover.
    pub refused: Vec<Veto>,
    /// Engine panics caught and isolated during this pass. NOTE: isolation
    /// stops the unwind, but the process-global panic HOOK still runs first
    /// — server embedders see a backtrace on stderr per caught panic
    /// (suppress via `std::panic::set_hook` in the application if needed;
    /// a library must not touch the global hook).
    pub panics: u8,
}

/// Run a closure with a panic boundary. `None` = the closure panicked.
fn run_isolated<T>(f: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(f)).ok()
}

/// Run one engine pass under the panic boundary, tallying an isolated panic
/// into `panics`. `None` = the engine panicked (already counted). This is the
/// single place `EngineOutcome::panics` is incremented, so the two engine call
/// sites cannot drift on how — or whether — a caught panic is recorded.
fn run_engine<T>(panics: &mut u8, engine: impl FnOnce() -> T) -> Option<T> {
    let outcome = run_isolated(engine);
    if outcome.is_none() {
        // a caught panic is isolated but counted — the ladder surfaces the
        // running tally as `PipelineTrace::engine_panics`.
        *panics += 1;
    }
    outcome
}

/// Which symbologies a decode pass tries — the cost lever: every extra
/// format family is another detector walking the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormatFilter {
    /// Everything rxing supports + rqrr.
    All,
    /// QR · Micro QR · rMQR (+ rqrr) — the artistic-recovery stages are
    /// QR-calibrated; paying the 1D/PDF417/DataMatrix detectors on every
    /// deep rung starves the budget for nothing.
    QrFamily,
    /// Exactly one symbology — the scoring fast path (30 stress cells ×
    /// every decoder is 1× vs ~5× scan cost, and a cross-symbology text
    /// match would be a false survival signal anyway).
    Only(Symbology),
}

/// One decode pass: every enabled engine, all symbologies.
#[cfg(test)]
pub(crate) fn decode_all(luma: &LumaImage) -> EngineOutcome {
    decode_filtered(luma, FormatFilter::All)
}

/// Decode under a format filter.
pub(crate) fn decode_filtered(luma: &LumaImage, filter: FormatFilter) -> EngineOutcome {
    let mut outcome = EngineOutcome::default();

    #[cfg(feature = "engine-rxing")]
    if let Some(found) = run_engine(&mut outcome.panics, || rxing_engine::decode(luma, filter)) {
        outcome.detections.extend(found);
    }

    // a single-symbology pass (score cells, envelope probes) discards the
    // rescue candidates: their orientation is never worth an RS there
    #[cfg(feature = "engine-rqrr")]
    if matches!(
        filter,
        FormatFilter::All | FormatFilter::QrFamily | FormatFilter::Only(Symbology::QrCode)
    ) && let Some(pass) = run_engine(&mut outcome.panics, || {
        rqrr_engine::decode(luma, !matches!(filter, FormatFilter::Only(_)))
    }) {
        outcome.detections.extend(pass.found);
        outcome.rescue.extend(pass.rescue);
        outcome.refused.extend(pass.refused);
    }

    outcome
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

    use super::*;
    use crate::input::LumaImage;
    use crate::report::{Charset, EcLevel, EngineKind};

    /// Render a QR to luma bytes module-by-module (no image-crate coupling):
    /// `scale` px per module + 4-module quiet zone. Returns the symbol version.
    fn qr_luma(content: &[u8], ec: qrcode::EcLevel, scale: u32) -> (LumaImage, u8) {
        let code = qrcode::QrCode::with_error_correction_level(content, ec).unwrap();
        let modules = code.width();
        let colors = code.to_colors();
        let qz = 4u32;
        let side = (u32::try_from(modules).unwrap() + 2 * qz) * scale;
        let mut data = vec![255u8; (side * side) as usize];
        for my in 0..modules {
            for mx in 0..modules {
                if colors[my * modules + mx] == qrcode::Color::Dark {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let x = (u32::try_from(mx).unwrap() + qz) * scale + dx;
                            let y = (u32::try_from(my).unwrap() + qz) * scale + dy;
                            data[(y * side + x) as usize] = 0;
                        }
                    }
                }
            }
        }
        let version = match code.version() {
            qrcode::Version::Normal(v) => u8::try_from(v).unwrap(),
            qrcode::Version::Micro(_) => 0,
        };
        (LumaImage::new(data, side, side), version)
    }

    #[cfg(feature = "engine-rxing")]
    #[test]
    fn rxing_decodes_utf8_qr_with_text_and_ec() {
        let payload = "héllo → 🦋";
        let (luma, _) = qr_luma(payload.as_bytes(), qrcode::EcLevel::Q, 4);
        let outcome = decode_all(&luma);
        let d = outcome
            .detections
            .iter()
            .find(|d| d.engine == EngineKind::Rxing)
            .expect("rxing must decode a clean generated QR");
        let (text, _) = charset::resolve(&d.raw);
        assert_eq!(text, payload);
        assert_eq!(d.raw, payload.as_bytes());
        assert_eq!(d.ec, Some(EcLevel::Q));
        assert_eq!(outcome.panics, 0);
    }

    #[cfg(feature = "engine-rqrr")]
    #[test]
    fn rqrr_provides_geometry_version_ec_mask() {
        let (luma, version) = qr_luma(b"https://qrcode-ai.com/scan", qrcode::EcLevel::Q, 4);
        let outcome = decode_all(&luma);
        let d = outcome
            .detections
            .iter()
            .find(|d| d.engine == EngineKind::Rqrr)
            .expect("rqrr must decode a clean generated QR");
        assert_eq!(d.version, Some(version));
        assert_eq!(d.ec, Some(EcLevel::Q), "rqrr ecc_level 0-3 mapping");
        let mask = d.mask.expect("rqrr reports the mask");
        assert!(mask <= 7, "mask {mask}");
        let corners = d.corners.expect("rqrr reports bounds");
        let side = luma.width() as f32;
        for p in corners {
            assert!(
                p.x >= 0.0 && p.x < side && p.y >= 0.0 && p.y < side,
                "{p:?}"
            );
        }
    }

    #[cfg(feature = "engine-rqrr")]
    #[test]
    fn rqrr_ec_mapping_pinned_on_l_too() {
        let (luma, _) = qr_luma(b"ec level L pin", qrcode::EcLevel::L, 4);
        let outcome = decode_all(&luma);
        let d = outcome
            .detections
            .iter()
            .find(|d| d.engine == EngineKind::Rqrr)
            .expect("decode");
        assert_eq!(d.ec, Some(EcLevel::L));
    }

    #[cfg(all(feature = "engine-rxing", feature = "engine-rqrr"))]
    #[test]
    fn engines_agree_on_raw_payload_bytes() {
        let payload = "parity check 1234";
        let (luma, _) = qr_luma(payload.as_bytes(), qrcode::EcLevel::M, 4);
        let outcome = decode_all(&luma);
        let rxing_raw = outcome
            .detections
            .iter()
            .find(|d| d.engine == EngineKind::Rxing)
            .map(|d| d.raw.clone())
            .expect("rxing");
        let rqrr_raw = outcome
            .detections
            .iter()
            .find(|d| d.engine == EngineKind::Rqrr)
            .map(|d| d.raw.clone())
            .expect("rqrr");
        assert_eq!(rxing_raw, rqrr_raw);
        assert_eq!(rxing_raw, payload.as_bytes());
    }

    #[cfg(feature = "engine-rqrr")]
    #[test]
    fn latin1_byte_mode_payload_resolves_via_raw_path() {
        // "Éé" in windows-1252 — invalid UTF-8, the rqrr decode() trap case.
        let (luma, _) = qr_luma(&[0xC9, 0xE9], qrcode::EcLevel::M, 4);
        let outcome = decode_all(&luma);
        let d = outcome
            .detections
            .iter()
            .find(|d| d.engine == EngineKind::Rqrr)
            .expect("rqrr must decode byte-mode latin1");
        assert_eq!(d.raw, vec![0xC9, 0xE9]);
        let (text, cs) = charset::resolve(&d.raw);
        assert_eq!(text, "Éé");
        assert_eq!(cs, Charset::Latin1);
    }

    #[test]
    fn charset_resolution_order_utf8_sjis_latin1() {
        let (t, c) = charset::resolve("plain ascii".as_bytes());
        assert_eq!((t.as_str(), c), ("plain ascii", Charset::Utf8));

        let (t, c) = charset::resolve("héllo🦋".as_bytes());
        assert_eq!((t.as_str(), c), ("héllo🦋", Charset::Utf8));

        // こんにちは in Shift-JIS
        let sjis: &[u8] = &[0x82, 0xB1, 0x82, 0xF1, 0x82, 0xC9, 0x82, 0xBF, 0x82, 0xCD];
        let (t, c) = charset::resolve(sjis);
        assert_eq!((t.as_str(), c), ("こんにちは", Charset::ShiftJis));

        let (t, c) = charset::resolve(&[0xC9, 0xE9]);
        assert_eq!((t.as_str(), c), ("Éé", Charset::Latin1));
    }

    #[cfg(all(feature = "engine-rxing", feature = "engine-rqrr"))]
    #[test]
    fn stress_cells_match_by_content_key_not_by_text() {
        // A stress cell survives when an engine hit carries the scored
        // CONTENT. Every hit of an ECI-4 [A1] symbol — clean, rotated,
        // blurred, from either engine, located or only signalled — is
        // admitted by the detection's key, while its heuristic text ('¡')
        // differs from the detection's ('Ą'), and an unmarked [A1] hit is
        // refused. (The cell matcher must compare `ContentKey::admits`.)
        let symbol = |eci: bool| {
            crate::rescue::test_symbols::from_segments(
                qrcode::Version::Normal(3),
                qrcode::EcLevel::M,
                |b| {
                    if eci {
                        b.push_eci_designator(4)?;
                    }
                    b.push_byte_data(&[0xA1])
                },
            )
        };
        let key = content::ContentKey {
            raw: vec![0xA1],
            eci: EciEvidence::Located(vec![content::EciMark { value: 4, start: 0 }]),
            fnc1: Fnc1::None,
        };
        assert_eq!(content::read(&key.raw, &key.eci).text, "\u{104}");
        let luma = symbol(true);
        let cells = [
            luma.clone(),
            crate::score::warp::rotate(&luma, 20.0),
            crate::transform::gaussian_blur(&luma, 1.5),
        ];
        for (i, cell) in cells.iter().enumerate() {
            let outcome = decode_filtered(cell, FormatFilter::Only(Symbology::QrCode));
            assert!(!outcome.detections.is_empty(), "cell {i} decodes");
            for d in &outcome.detections {
                assert!(key.admits(d), "cell {i} {:?}: {:?}", d.engine, d.eci);
                assert_eq!(charset::resolve(&d.raw).0, "¡");
            }
        }
        let unmarked = decode_filtered(&symbol(false), FormatFilter::Only(Symbology::QrCode));
        assert!(!unmarked.detections.is_empty());
        assert!(unmarked.detections.iter().all(|d| !key.admits(d)));
    }

    #[test]
    fn panics_are_isolated_not_propagated() {
        let caught: Option<u8> = run_isolated(|| panic!("engine exploded"));
        assert!(caught.is_none());
        let fine = run_isolated(|| 7u8);
        assert_eq!(fine, Some(7));
    }

    #[test]
    fn run_engine_tallies_each_isolated_panic_by_exactly_one() {
        // Pins the `EngineOutcome::panics` counter that `decode_filtered`
        // threads for both engines (mod.rs `*panics += 1`). Traps the shard-0
        // survivors: `+=` → `-=` (0u8 - 1 underflows → the pass panics, the
        // isolation contract broken) and `+=` → `*=` (0 * 1 stays 0, a caught
        // panic goes unrecorded). A clean pass must NOT touch the counter, and
        // a second panic must ACCUMULATE — pinning `+=` (not `=`) and step 1.
        let mut panics = 0u8;

        let ok = run_engine(&mut panics, || 7u8);
        assert_eq!(ok, Some(7), "a surviving engine returns its value");
        assert_eq!(panics, 0, "a clean pass tallies nothing");

        let boom: Option<u8> = run_engine(&mut panics, || panic!("engine exploded"));
        assert!(boom.is_none(), "an isolated panic yields None");
        assert_eq!(panics, 1, "one caught panic is exactly one tally");

        let again: Option<u8> = run_engine(&mut panics, || panic!("engine exploded again"));
        assert!(again.is_none());
        assert_eq!(panics, 2, "a second caught panic accumulates (+=, not =)");
    }

    #[test]
    fn a_veto_covers_the_unparsed_qr_reads_of_its_content_only() {
        let hit = |raw: &[u8], eci: EciEvidence, fnc1: Fnc1, engine: EngineKind| RawDetection {
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
            engine,
        };
        // ECI 1000000 + "abc": the port signals a designator
        let port = Veto::port(PortRead {
            bytes: b"abc".to_vec(),
            signalled: true,
            fnc1: Fnc1::None,
        });
        let signalled = hit(
            b"abc",
            EciEvidence::Signalled,
            Fnc1::None,
            EngineKind::Rxing,
        );
        assert!(port.refuses(&signalled));
        // rqrr's unverified read of the same bytes observed nothing that
        // tells it apart: covered
        let unobserved = hit(
            b"abc",
            EciEvidence::Unobserved,
            Fnc1::None,
            EngineKind::Rqrr,
        );
        assert!(port.refuses(&unobserved));
        // a plain symbol, a GS1 one, other bytes: other contents
        for other in [
            hit(b"abc", EciEvidence::NoEci, Fnc1::None, EngineKind::Rxing),
            hit(
                b"abc",
                EciEvidence::Signalled,
                Fnc1::First,
                EngineKind::Rxing,
            ),
            hit(
                b"abd",
                EciEvidence::Signalled,
                Fnc1::None,
                EngineKind::Rxing,
            ),
        ] {
            assert!(!port.refuses(&other), "{other:?}");
        }
        // a parsed read carries a well-formed stream; Micro QR another symbol
        let mut parsed = signalled.clone();
        parsed.parsed = true;
        let mut micro = signalled.clone();
        micro.symbology = Symbology::MicroQrCode;
        assert!(!port.refuses(&parsed) && !port.refuses(&micro));
        // rqrr's own bytes cover rqrr's unparsed reads alone
        let rqrr = Veto::Rqrr(b"abc".to_vec());
        assert!(rqrr.refuses(&unobserved));
        assert!(!rqrr.refuses(&signalled), "never the port's read");
        // detections: the content agrees, or rqrr alone made it
        let key = |eci: EciEvidence| ContentKey {
            raw: b"abc".to_vec(),
            eci,
            fnc1: Fnc1::None,
        };
        assert!(port.refuses_content(&key(EciEvidence::Signalled), &[EngineKind::Rxing]));
        assert!(!port.refuses_content(&key(EciEvidence::NoEci), &[EngineKind::Rxing]));
        assert!(rqrr.refuses_content(&key(EciEvidence::Unobserved), &[EngineKind::Rqrr]));
        assert!(!rqrr.refuses_content(
            &key(EciEvidence::NoEci),
            &[EngineKind::Rxing, EngineKind::Rqrr]
        ));
    }
}
