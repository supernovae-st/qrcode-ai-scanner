//! Decode engines — isolated wrappers around third-party decoders.
//!
//! Contract: engines NEVER panic outward (`catch_unwind` boundary), never
//! decide text (they report payload bytes + what they saw of the ECI
//! declarations; the interpretation happens once, in `content`), and only
//! report what they actually measured — no stubs.

pub(crate) mod charset;
pub(crate) mod content;
pub(crate) mod locate;
pub(crate) mod reconcile;
#[cfg(feature = "engine-rqrr")]
mod rqrr_engine;
#[cfg(feature = "engine-rxing")]
mod rxing_engine;

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::input::LumaImage;
use crate::report::{EcLevel, EngineKind, Point, Symbology};
use crate::rescue::bitstream::PortRead;
use content::{ContentKey, EciEvidence, EngineReading, Fnc1, Segments};
use locate::{LocateHint, Located};

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
    /// FNC1 account is exposed, not stated by an engine. A refusal withholds
    /// only reads that are not parsed — equal bytes from a parsed stream
    /// belong to another, well-formed symbol.
    pub parsed: bool,
    /// Producing engine.
    pub engine: EngineKind,
    /// The Kanji/Hanzi segments the scanner's parse observed (`NotObserved`
    /// when no parse read the stream).
    pub segments: Segments,
    /// The engine's own mode-aware reading of a QR-family port read
    /// (spec/01 rule 1e); `None` elsewhere.
    pub engine_reading: EngineReading,
    /// The corner points of the symbol the read came from — never
    /// published, they keep the reads of different symbols apart: rxing's
    /// QR-family result points, rqrr's grid bounds. Attempt space here.
    pub location: Option<[Point; 4]>,
    /// What the re-read needs, when it may run for this read: a port read
    /// whose modifier leaves its declarations unverified, an rqrr read whose
    /// grid decided nothing. Attempt space.
    pub locate: Option<LocateHint>,
    /// The walk publishes this read only once a read at an independent scale
    /// confirms it (07-pipeline § Acceptance): a symbology the readers invent
    /// from module noise, its payload guarded by a check digit or a checksum
    /// alone (UPC-E, `DataBar`, `DataBar` Expanded), or a PDF417 symbol at
    /// error-correction level 0 or 1 (two or four EC codewords) or at no
    /// stated level. Set by the rxing adapter only.
    pub needs_confirmation: bool,
    /// The correction margin of the read's own received word, when the
    /// scanner error-corrected that word itself (the rqrr adapter's
    /// re-derivation); `Unknown` otherwise — an rxing read gets one only from
    /// a re-read that locates it.
    pub margin: crate::rescue::Margin,
}

/// The content rxing's zxing-cpp port reports for a stream: its bytes, a
/// designator signalled iff the port read one, its FNC1 position.
pub(crate) fn port_key(read: PortRead) -> ContentKey {
    ContentKey {
        raw: read.bytes,
        eci: if read.signalled {
            EciEvidence::Signalled
        } else {
            EciEvidence::NoEci
        },
        fnc1: read.fnc1,
    }
}

/// One refused QR stream (ISO 18004 defines no symbol for it), the names it
/// withholds by and where it was refused (07-pipeline property 6
/// § Refusal). A refusal is a content, never bytes alone: a well-formed
/// symbol with equal bytes is another content and keeps its reads; and it
/// is a place: only reads lying inside the refused symbol are withheld. The
/// walk keeps one record per stream and names, wherever it is refused
/// (`reconcile::ReadStore::refuse`) — never compared whole: the place is a
/// rescaled measurement.
#[derive(Debug, Clone)]
pub(crate) struct Refusal {
    /// What rxing's port reports for the stream ([`port_key`]): withholds
    /// the unparsed QR Code reads whose account matches.
    pub port: Option<ContentKey>,
    /// The bytes rqrr decoded from the stream (its own conversion):
    /// withholds unparsed rqrr reads of them, nothing else.
    pub rqrr: Option<Vec<u8>>,
    /// Where the refused symbol is: its grid's corners, or the refused
    /// read's points (attempt space until the ladder rescales it).
    pub place: Option<[Point; 4]>,
}

impl Refusal {
    /// A refused stream's record; `None` when it names nothing to withhold.
    pub(crate) fn new(
        port: Option<PortRead>,
        rqrr: Option<Vec<u8>>,
        place: Option<[Point; 4]>,
    ) -> Option<Self> {
        (port.is_some() || rqrr.is_some()).then(|| Self {
            port: port.map(port_key),
            rqrr,
            place,
        })
    }
}

/// The re-read of one read in its attempt image, under its own panic
/// boundary: a panic is a re-read that could not verify (no engine ran, so
/// nothing is tallied).
pub(crate) fn reread(img: &LumaImage, hint: &LocateHint, payload: &[u8]) -> Located {
    run_isolated(|| locate::locate(img, hint, payload)).unwrap_or(Located::Unverified)
}

/// Aggregated result of one pass over all enabled engines.
#[derive(Debug, Clone, Default)]
pub(crate) struct EngineOutcome {
    /// Every detection from every engine (deduplication is the ladder's job).
    pub detections: Vec<RawDetection>,
    /// Grids detected but NOT decoded (rqrr `get_raw_data` ok, RS failed) —
    /// the S5 rescue inputs. Corners are in ATTEMPT space here.
    pub rescue: Vec<crate::rescue::RescueCandidate>,
    /// QR streams proven malformed in this pass: decodes the rqrr adapter
    /// refused because their own grid parses malformed. The ladder withholds
    /// the reads they cover.
    pub refused: Vec<Refusal>,
    /// Engine panics caught and isolated during this pass. NOTE: isolation
    /// stops the unwind, but the process-global panic HOOK still runs first
    /// — server embedders see a backtrace on stderr per caught panic
    /// (suppress via `std::panic::set_hook` in the application if needed;
    /// a library must not touch the global hook).
    pub panics: u8,
}

#[cfg(test)]
impl RawDetection {
    /// A QR Code read with nothing measured but its content.
    pub(crate) fn qr(raw: &[u8], eci: EciEvidence, fnc1: Fnc1, engine: EngineKind) -> Self {
        Self {
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
            segments: Segments::NotObserved,
            engine_reading: EngineReading::None,
            location: None,
            locate: None,
            needs_confirmation: false,
            margin: crate::rescue::Margin::Unknown,
        }
    }
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

#[cfg(all(test, feature = "engine-rxing"))]
thread_local! {
    /// Test-only: while `Some`, the rxing pass of every single-symbology
    /// decode (score cells, envelope probes) panics inside the engine
    /// boundary — the real catch-and-tally path, no decoder bug needed —
    /// and the injected panics are counted here.
    pub(crate) static PROBE_PANICS: std::cell::Cell<Option<u32>> =
        const { std::cell::Cell::new(None) };
}

/// Test-only: the injected panic of [`PROBE_PANICS`].
#[cfg(all(test, feature = "engine-rxing"))]
fn inject_probe_panic(filter: FormatFilter) {
    if !matches!(filter, FormatFilter::Only(_)) {
        return;
    }
    if let Some(count) = PROBE_PANICS.with(std::cell::Cell::get) {
        PROBE_PANICS.with(|injected| injected.set(Some(count + 1)));
        panic!("injected decoder panic");
    }
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
    if let Some(found) = run_engine(&mut outcome.panics, || {
        #[cfg(test)]
        inject_probe_panic(filter);
        rxing_engine::decode(luma, filter)
    }) {
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
        // refused. (The cell matcher compares `ContentKey::admits`.)
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
        let reading = content::read(
            &key.raw,
            &key.eci,
            &Segments::NotObserved,
            EngineReading::None,
            true,
        );
        assert_eq!(reading.text, "\u{104}");
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
    fn a_refusal_names_the_port_content_and_rqrrs_bytes() {
        // ECI 1000000 + "abc": the port signals a designator
        let signalled = PortRead {
            bytes: b"abc".to_vec(),
            signalled: true,
            fnc1: Fnc1::None,
        };
        assert_eq!(
            port_key(signalled.clone()),
            ContentKey {
                raw: b"abc".to_vec(),
                eci: EciEvidence::Signalled,
                fnc1: Fnc1::None,
            }
        );
        let plain = PortRead {
            signalled: false,
            fnc1: Fnc1::Second,
            ..signalled.clone()
        };
        assert_eq!(port_key(plain).eci, EciEvidence::NoEci);
        // one record per stream, both names; nothing named, no record
        let place = Some([Point { x: 0.0, y: 0.0 }; 4]);
        let refusal = Refusal::new(Some(signalled), Some(b"abc".to_vec()), place)
            .expect("names to withhold by");
        assert_eq!(refusal.rqrr.as_deref(), Some(&b"abc"[..]));
        assert_eq!(refusal.place, place);
        assert!(Refusal::new(None, None, place).is_none());
    }
}
