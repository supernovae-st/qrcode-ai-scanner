//! Alpha-transparency handling — the flatten and the placement envelope.
//!
//! A transparent asset has no ONE background: the same pixels composite to
//! a different image on every host surface. Pre-0.9 the engine read the
//! STORED RGB under the transparency — an exporter-dependent verdict
//! (canvas exports store black under full transparency, image editors
//! often white; the same visual design scored 100, 74 or nothing at all
//! depending on the exporting tool). The flatten pins the verdict to a
//! DECLARED background; the envelope answers the product question "over
//! which backgrounds does this design keep decoding" with tested probes,
//! never interpolation. Everything here is integer, pass-bounded and
//! deterministic — same input, same bytes, same report.

use web_time::Instant;

use crate::error::{Result, ScanError};
use crate::input::{LumaImage, bt601};
use crate::ladder::{AlphaBackground, CancelToken};
use crate::report::{AlphaEnvelope, AlphaMode, AlphaPlacement, AlphaProbe, AlphaReport, Symbology};
use crate::transform::{self, SourcePlanes};

/// Envelope probes run on a ≤512px premultiplied base — one probe costs
/// the same class as one score stress cell.
const ENVELOPE_BASE_SIDE: u32 = 512;

/// The fixed neutral-background rungs, dark → light.
const ENVELOPE_RUNGS: [u8; 5] = [0, 64, 128, 192, 255];

/// Straight-alpha composite of one channel over a background value —
/// integer round-half-up: `(fg·a + bg·(255−a) + 127) / 255`.
#[inline]
pub(crate) fn composite(fg: u8, bg: u8, a: u8) -> u8 {
    let n = u32::from(fg) * u32::from(a) + u32::from(bg) * (255 - u32::from(a)) + 127;
    // 0..=255 by construction: fg·a + bg·(255−a) ≤ 255·255
    u8::try_from(n / 255).unwrap_or(u8::MAX)
}

/// The `auto` background decision: dark content reads best over white,
/// light content over black — measured on the design's own visible pixels.
pub(crate) fn auto_background(content_luma: u8) -> [u8; 3] {
    if content_luma < 128 { [255; 3] } else { [0; 3] }
}

/// Cheap routing test — does the buffer carry ANY transparency? Early-exits
/// on the first non-opaque byte, so the common fully-opaque RGBA input pays
/// one alpha-byte compare per pixel and nothing else.
pub(crate) fn has_transparency(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).any(|px| px[3] < 255)
}

/// One-pass content statistics over straight RGBA.
pub(crate) struct ContentStats {
    /// Pixels with alpha < 255.
    pub transparent: u64,
    /// Alpha-weighted mean BT.601 luma of the content — a half-transparent
    /// module contributes half its luma. 0 when the image is fully
    /// transparent (flattens over white, decodes nothing — deterministic).
    pub content_luma: u8,
}

/// Count transparency + measure the content's alpha-weighted mean luma.
pub(crate) fn content_stats(rgba: &[u8]) -> ContentStats {
    let mut transparent = 0u64;
    let mut weight = 0u64;
    let mut weighted_luma = 0u64;
    for px in rgba.chunks_exact(4) {
        let a = u64::from(px[3]);
        if a < 255 {
            transparent += 1;
        }
        weight += a;
        weighted_luma += u64::from(bt601(px[0], px[1], px[2])) * a;
    }
    // round-half-up mean; ≤255 because every luma term is ≤255·weight —
    // zero weight (fully transparent) reads a deterministic 0
    let content_luma = (weighted_luma + weight / 2)
        .checked_div(weight)
        .map_or(0, |mean| u8::try_from(mean).unwrap_or(u8::MAX));
    ContentStats {
        transparent,
        content_luma,
    }
}

/// Per-scan alpha context — built by `transform::normalize_with` when the
/// input carried transparent pixels under a flattening mode, consumed by
/// the zero-detection fallback, the envelope, and the report block.
#[derive(Debug, Clone)]
pub(crate) struct AlphaContext {
    /// Straight (non-premultiplied) alpha plane at native resolution.
    alpha: Vec<u8>,
    /// BT.601 luma of the straight RGB — the content under the alpha.
    fg_luma: Vec<u8>,
    width: u32,
    height: u32,
    /// Fraction of pixels with alpha < 255, rounded to 3 decimals.
    coverage: f32,
    /// Alpha-weighted mean luma of the visible content.
    content_luma: u8,
    /// Requested mode (config echo for the report).
    mode: AlphaBackground,
    /// Background under the CURRENT planes (updated when the fallback
    /// rescues the scan).
    background: [u8; 3],
}

impl AlphaContext {
    #[expect(
        clippy::cast_precision_loss,
        reason = "transparent/total are bounded by Limits::max_pixels (64MP < 2^52) — exact in f64"
    )]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a 0..=1 ratio rounded to 3 decimals fits f32 exactly"
    )]
    pub(crate) fn new(
        alpha: Vec<u8>,
        fg_luma: Vec<u8>,
        width: u32,
        height: u32,
        stats: &ContentStats,
        mode: AlphaBackground,
        background: [u8; 3],
    ) -> Self {
        let total = u64::from(width) * u64::from(height);
        let coverage = if total == 0 {
            0.0
        } else {
            ((stats.transparent as f64 / total as f64) * 1000.0).round() / 1000.0
        };
        Self {
            alpha,
            fg_luma,
            width,
            height,
            coverage: coverage as f32,
            content_luma: stats.content_luma,
            mode,
            background,
        }
    }

    /// Luma-only planes flattened over the OPPOSITE neutral background —
    /// the zero-detection retry. `Some` only under `auto` (a forced
    /// background is the host's explicit truth; second-guessing it would
    /// be score surgery).
    pub(crate) fn opposite_planes(&self) -> Option<(SourcePlanes, [u8; 3])> {
        if self.mode != AlphaBackground::Auto {
            return None;
        }
        let bg = if self.background == [255; 3] {
            [0; 3]
        } else {
            [255; 3]
        };
        let luma: Vec<u8> = self
            .fg_luma
            .iter()
            .zip(&self.alpha)
            .map(|(&fg, &a)| composite(fg, bg[0], a))
            .collect();
        // Luma-only on purpose: the opposite-background RGB planes are not
        // retained, so the retry runs the grayscale ladder (pyramid ·
        // direct · otsu/invert · deep). The channel-split rungs need color
        // no fallback class has ever required.
        Some((
            SourcePlanes::luma_only(LumaImage::new(luma, self.width, self.height)),
            bg,
        ))
    }

    /// Record that the opposite background produced the detections.
    pub(crate) fn adopt_background(&mut self, background: [u8; 3]) {
        self.background = background;
    }

    /// The placement envelope: five fixed neutral rungs + one bisection
    /// probe per decoded/undecoded boundary, all on a premultiplied ≤512px
    /// base (composite-then-downscale ≡ downscale-then-composite only in
    /// premultiplied space — box-averaging straight alpha would bleed
    /// background into half-covered edge pixels).
    ///
    /// Probes measure in the symbol's OWN decode class — the score's
    /// stress-cell philosophy (direct → otsu → the deep rung the base
    /// needed), calibrated on the design over its PRIMARY background. A
    /// raw direct-only decode would read an all-fail envelope for an
    /// artistic design that legitimately decodes through a boost rung.
    ///
    /// `Ok(None)` when the budget ran out mid-sweep — a partial envelope
    /// would read as a verdict about backgrounds that were never tested.
    ///
    /// # Errors
    /// `QRS-005` when cancellation fires between probes (the scan-wide
    /// cancellation contract).
    pub(crate) fn envelope(
        &self,
        expected_text: &str,
        symbology: Symbology,
        cancel: &CancelToken,
        deadline: Option<Instant>,
    ) -> Result<Option<AlphaEnvelope>> {
        if cancel.is_cancelled() {
            return Err(ScanError::Cancelled);
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(None);
        }
        let premultiplied: Vec<u8> = self
            .fg_luma
            .iter()
            .zip(&self.alpha)
            .map(|(&fg, &a)| composite(fg, 0, a))
            .collect();
        let pm = transform::downscale_to(
            &LumaImage::new(premultiplied, self.width, self.height),
            ENVELOPE_BASE_SIDE,
        );
        let alpha = transform::downscale_to(
            &LumaImage::new(self.alpha.clone(), self.width, self.height),
            ENVELOPE_BASE_SIDE,
        );
        let over = |bg: u8| -> LumaImage {
            let luma: Vec<u8> = pm
                .data()
                .iter()
                .zip(alpha.data())
                .map(|(&p, &a)| p.saturating_add(composite(0, bg, a)))
                .collect();
            LumaImage::new(luma, pm.width(), pm.height())
        };
        // Calibrate once on the design over its primary background (the
        // image the verdict stands on, at probe scale) — colored custom
        // backgrounds calibrate at their BT.601 luma (probes are a luma-
        // space sweep by construction).
        let base = over(bt601(
            self.background[0],
            self.background[1],
            self.background[2],
        ));
        let cell = crate::score::CellProbe::calibrate(&base, expected_text, symbology).unwrap_or(
            crate::score::CellProbe {
                rung: None,
                symbology,
            },
        );

        let probe = |bg: u8| -> Result<Option<bool>> {
            if cancel.is_cancelled() {
                return Err(ScanError::Cancelled);
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(None);
            }
            Ok(Some(crate::score::cell_passes(
                &over(bg),
                expected_text,
                cell,
            )))
        };

        let mut probes: Vec<(u8, bool)> = Vec::with_capacity(ENVELOPE_RUNGS.len() + 4);
        for bg in ENVELOPE_RUNGS {
            match probe(bg)? {
                Some(decoded) => probes.push((bg, decoded)),
                None => return Ok(None),
            }
        }
        // One bisection probe per verdict boundary tightens each band edge
        // from ±64 to ±32 — every reported endpoint stays a TESTED value.
        let mut refinements = Vec::new();
        for pair in probes.windows(2) {
            if pair[0].1 != pair[1].1 {
                let mid = u8::midpoint(pair[0].0, pair[1].0);
                match probe(mid)? {
                    Some(decoded) => refinements.push((mid, decoded)),
                    None => return Ok(None),
                }
            }
        }
        probes.extend(refinements);
        probes.sort_unstable_by_key(|&(bg, _)| bg);

        let (safe_luma, placement) = classify(&probes);
        Ok(Some(AlphaEnvelope {
            probes: probes
                .into_iter()
                .map(|(background_luma, decoded)| AlphaProbe {
                    background_luma,
                    decoded,
                })
                .collect(),
            safe_luma,
            placement,
        }))
    }

    /// Whether the envelope applies (it measures a flatten, never the
    /// dropped-alpha escape hatch — that mode carries no context at all).
    pub(crate) fn into_report(
        self,
        fallback_used: bool,
        envelope: Option<AlphaEnvelope>,
    ) -> AlphaReport {
        let mode = match self.mode {
            // `none` never builds a context (no block on the wire) — the
            // arm exists only to keep the match total
            AlphaBackground::Auto | AlphaBackground::None => AlphaMode::Auto,
            AlphaBackground::White => AlphaMode::White,
            AlphaBackground::Black => AlphaMode::Black,
            AlphaBackground::Custom(_) => AlphaMode::Custom,
        };
        AlphaReport {
            coverage: self.coverage,
            content_luma: self.content_luma,
            mode,
            background: background_name(self.background),
            fallback_used,
            envelope,
        }
    }
}

/// Wire spelling of a resolved background.
fn background_name(bg: [u8; 3]) -> String {
    match bg {
        [255, 255, 255] => "white".to_owned(),
        [0, 0, 0] => "black".to_owned(),
        [r, g, b] => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

/// Contiguous decoded bands + the placement verdict they spell.
fn classify(probes: &[(u8, bool)]) -> (Vec<[u8; 2]>, AlphaPlacement) {
    // probes are sorted by background luma, so contiguity IS adjacency: a
    // band extends exactly while consecutive probes keep decoding
    let mut bands: Vec<[u8; 2]> = Vec::new();
    let mut previous_decoded = false;
    for &(bg, decoded) in probes {
        if decoded {
            match bands.last_mut() {
                Some(band) if previous_decoded => band[1] = bg,
                _ => bands.push([bg, bg]),
            }
        }
        previous_decoded = decoded;
    }
    let placement = if bands.is_empty() {
        AlphaPlacement::None
    } else if probes.iter().all(|&(_, decoded)| decoded) {
        AlphaPlacement::Any
    } else if bands.len() == 1 && bands[0][1] == 255 {
        AlphaPlacement::LightOnly
    } else if bands.len() == 1 && bands[0][0] == 0 {
        AlphaPlacement::DarkOnly
    } else {
        AlphaPlacement::Mixed
    };
    (bands, placement)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn composite_is_exact_at_the_anchors() {
        // fully opaque keeps the foreground · fully transparent takes the
        // background — the two ends every exporter agrees on
        assert_eq!(composite(40, 255, 255), 40);
        assert_eq!(composite(40, 255, 0), 255);
    }

    #[test]
    fn composite_half_alpha_pins_the_rounding() {
        // (0·128 + 255·127 + 127)/255 = 32512/255 = 127.498… → 127; a
        // rounding mutation (drop the +127 · +255) moves it to 126/128
        assert_eq!(composite(0, 255, 128), 127);
        // (255·128 + 0 + 127)/255 = 32767/255 = 128.498… → 128
        assert_eq!(composite(255, 0, 128), 128);
    }

    #[test]
    fn auto_background_flips_at_the_midpoint() {
        assert_eq!(auto_background(0), [255; 3]);
        assert_eq!(auto_background(127), [255; 3]);
        assert_eq!(auto_background(128), [0; 3]);
        assert_eq!(auto_background(255), [0; 3]);
    }

    #[test]
    fn content_stats_weights_luma_by_alpha() {
        // one opaque black px + one fully transparent white px: the white
        // is INVISIBLE — content luma must read 0, not 127.
        let rgba = [0u8, 0, 0, 255, 255, 255, 255, 0];
        let stats = content_stats(&rgba);
        assert_eq!(stats.transparent, 1);
        assert_eq!(stats.content_luma, 0);
    }

    #[test]
    fn content_stats_semi_transparent_contributes_proportionally() {
        // a half-alpha white px alone: mean over the VISIBLE mass is white
        let rgba = [255u8, 255, 255, 128];
        let stats = content_stats(&rgba);
        assert_eq!(stats.transparent, 1);
        assert_eq!(stats.content_luma, 255);
    }

    #[test]
    fn content_stats_fully_transparent_is_zero_luma() {
        let rgba = [200u8, 200, 200, 0];
        let stats = content_stats(&rgba);
        assert_eq!(stats.transparent, 1);
        assert_eq!(stats.content_luma, 0, "no visible mass → deterministic 0");
    }

    #[test]
    fn classify_all_decoded_is_any() {
        let probes: Vec<(u8, bool)> = ENVELOPE_RUNGS.iter().map(|&bg| (bg, true)).collect();
        let (bands, placement) = classify(&probes);
        assert_eq!(bands, vec![[0, 255]]);
        assert_eq!(placement, AlphaPlacement::Any);
    }

    #[test]
    fn classify_light_suffix_is_light_only() {
        let probes = vec![
            (0, false),
            (64, false),
            (96, false),
            (128, true),
            (192, true),
            (255, true),
        ];
        let (bands, placement) = classify(&probes);
        assert_eq!(bands, vec![[128, 255]]);
        assert_eq!(placement, AlphaPlacement::LightOnly);
    }

    #[test]
    fn classify_dark_prefix_is_dark_only() {
        let probes = vec![
            (0, true),
            (64, true),
            (128, false),
            (192, false),
            (255, false),
        ];
        let (bands, placement) = classify(&probes);
        assert_eq!(bands, vec![[0, 64]]);
        assert_eq!(placement, AlphaPlacement::DarkOnly);
    }

    #[test]
    fn classify_split_ends_are_mixed_with_two_bands() {
        let probes = vec![
            (0, true),
            (64, false),
            (128, false),
            (192, false),
            (255, true),
        ];
        let (bands, placement) = classify(&probes);
        assert_eq!(bands, vec![[0, 0], [255, 255]]);
        assert_eq!(placement, AlphaPlacement::Mixed);
    }

    #[test]
    fn classify_nothing_decoded_is_none() {
        let probes: Vec<(u8, bool)> = ENVELOPE_RUNGS.iter().map(|&bg| (bg, false)).collect();
        let (bands, placement) = classify(&probes);
        assert!(bands.is_empty());
        assert_eq!(placement, AlphaPlacement::None);
    }

    #[test]
    fn classify_mid_only_band_is_mixed() {
        // decodes only around the middle greys — neither end touched
        let probes = vec![
            (0, false),
            (64, true),
            (128, true),
            (192, false),
            (255, false),
        ];
        let (bands, placement) = classify(&probes);
        assert_eq!(bands, vec![[64, 128]]);
        assert_eq!(placement, AlphaPlacement::Mixed);
    }

    #[test]
    fn background_name_spells_white_black_and_hex() {
        assert_eq!(background_name([255; 3]), "white");
        assert_eq!(background_name([0; 3]), "black");
        assert_eq!(background_name([26, 26, 46]), "#1a1a2e");
    }
}
