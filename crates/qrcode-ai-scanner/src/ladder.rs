//! Deterministic decode ladder — S1 pyramid → S2 direct → S3 enhance → S4 deep.
//!
//! Replaces the v0.2 RNG brute force: the attempt sequence is a fixed,
//! declared order, so the same input under the same config always walks the
//! same path. Budget (wall clock, attempt-granular — an engine call is not
//! interruptible) and cooperative cancellation are checked between attempts.
//! Early exit happens at stage boundaries: a stage that finds something
//! still completes, so consensus data within that stage is preserved.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use web_time::Instant;

use crate::engine::{self, MaskedStream, RawDetection};
use crate::error::{Result, ScanError};
use crate::input::LumaImage;
use crate::report::{EcLevel, EngineKind, PipelineTrace, Point, StageTrace};
use crate::transform::{self, Channel, SourcePlanes};

/// Cooperative cancellation handle. Cheap to clone; `Send + Sync`.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A fresh, non-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation — honoured at the next attempt boundary.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Stress-scoring depth — how many cells each axis ramp runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScoreDepth {
    /// No scoring (camera frames).
    Off,
    /// Reduced ramps (upload tool).
    Reduced,
    /// Full ramps (generator quality gate).
    Full,
}

/// Stage-level scan configuration. Build from a profile preset, then adjust
/// public fields as needed (the struct is non-exhaustive: presets are the
/// only constructors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[expect(
    clippy::struct_excessive_bools,
    reason = "stage switches ARE independent booleans — a bitflags layer would obscure the API"
)]
pub struct ScanConfig {
    /// Wall-clock budget in milliseconds (`None` = unbounded).
    pub budget_ms: Option<u64>,
    /// S1 — decode a ≤`pyramid_side` downscale first (cheapest win).
    pub pyramid: bool,
    /// S2 — decode the full-resolution luma.
    pub direct: bool,
    /// S3 — enhance set: otsu · invert · contrast · R/G/B channels.
    pub enhance: bool,
    /// S4 — curated deep grid (size × contrast × binarization).
    pub deep: bool,
    /// Longest side of the S1 pyramid attempt.
    pub pyramid_side: u32,
    /// Hard cap on the longest side of ANY image handed to the engines —
    /// the wall-clock of one engine call is otherwise unbounded (a single
    /// rxing `TryHarder` pass on 64MP runs for minutes and is not
    /// interruptible). Detections are rescaled to original coordinates.
    pub max_engine_side: u32,
    /// Stress-scoring depth applied after a successful decode.
    pub score_depth: ScoreDepth,
}

impl ScanConfig {
    /// Everything on — the quality-gate profile (~4 s budget).
    #[must_use]
    pub fn full() -> Self {
        Self {
            budget_ms: Some(4_000),
            pyramid: true,
            direct: true,
            enhance: true,
            deep: true,
            pyramid_side: 512,
            max_engine_side: 2_048,
            score_depth: ScoreDepth::Full,
        }
    }

    /// Upload-tool profile: no deep grid (~800 ms budget).
    #[must_use]
    pub fn fast() -> Self {
        Self {
            budget_ms: Some(800),
            deep: false,
            score_depth: ScoreDepth::Reduced,
            ..Self::full()
        }
    }

    /// Camera-frame profile: pyramid + direct only (~80 ms budget).
    #[must_use]
    pub fn frame() -> Self {
        Self {
            budget_ms: Some(80),
            enhance: false,
            deep: false,
            score_depth: ScoreDepth::Off,
            ..Self::full()
        }
    }
}

/// Named scan profiles — the public selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ScanProfile {
    /// Full ladder + full stress score (generator quality gate).
    #[default]
    Full,
    /// Reduced ladder, reduced budget (upload tool).
    Fast,
    /// Per-frame camera decode: no scoring, tight budget.
    Frame,
    /// Explicit stage configuration.
    Custom(ScanConfig),
}

impl ScanProfile {
    /// Parse the cross-language wire name (`full` · `fast` · `frame`) — the
    /// ONE mapping every binding reuses (drift-proof).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "full" => Some(Self::Full),
            "fast" => Some(Self::Fast),
            "frame" => Some(Self::Frame),
            _ => None,
        }
    }

    pub(crate) fn config(self) -> ScanConfig {
        match self {
            Self::Full => ScanConfig::full(),
            Self::Fast => ScanConfig::fast(),
            Self::Frame => ScanConfig::frame(),
            Self::Custom(config) => config,
        }
    }
}

/// Hard cap on distinct merged detections — a tile of thousands of
/// micro-QRs is a CPU/report amplifier, not a use case.
const MAX_DETECTIONS: usize = 16;

/// One S4 deep-recovery transform recipe. The SAME enumeration drives the
/// ladder and the score probe ([`crate::score`]'s `CellProbe`) — a symbol
/// that only decodes via rung N must measure its stress margin in that
/// decode class (the round-2 "decodes but scores 0" regression).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Rung {
    /// Downscale → multiplicative contrast/brightness → blur — the v0.2
    /// tier-3 "known-good" set, empirically selected on the original
    /// 74-image artistic corpus: contrast crushing averages art texture
    /// into module means (probe-verified on the legacy corpus, 2026-06-11).
    Boost {
        resize: u32,
        contrast: f32,
        brightness: f32,
        blur: f32,
    },
    /// Morphological close of DARK structures at native resolution, then
    /// downscale. Blob/dot pixel styles read as gap-noise to grid sampling —
    /// growing dark blobs merges them into solid modules. Kernel scales with
    /// render size: `side/divisor` (odd, floor 3). Probe-calibrated on the
    /// qrcode-ai.com template corpus (R3 blitz, 2026-06-11): k=11 and k=13
    /// at 1024px decode the blob class, k≤9 never does.
    MorphCloseDark {
        /// Kernel scale: `k = side/divisor`.
        divisor: u32,
        /// Post-close downscale side (sampling-friendly resolution).
        target: u32,
    },
    /// The mirror for LIGHT blob structures (light-on-dark styles).
    MorphCloseLight {
        /// Kernel scale: `k = side/divisor`.
        divisor: u32,
        /// Post-close downscale side.
        target: u32,
    },
}

impl Rung {
    /// Build this rung's attempt image.
    pub(crate) fn apply(self, luma: &LumaImage) -> LumaImage {
        match self {
            Self::Boost {
                resize,
                contrast,
                brightness,
                blur,
            } => {
                let sized = if resize > 0 {
                    transform::downscale_to(luma, resize)
                } else {
                    luma.clone()
                };
                let boosted = transform::contrast_boost(&sized, contrast, brightness);
                if blur > 0.3 {
                    transform::gaussian_blur(&boosted, blur)
                } else {
                    boosted
                }
            }
            Self::MorphCloseDark { divisor, target } => {
                let closed = transform::min_filter(luma, morph_kernel(luma, divisor));
                transform::downscale_to(&closed, target)
            }
            Self::MorphCloseLight { divisor, target } => {
                let closed = transform::max_filter(luma, morph_kernel(luma, divisor));
                transform::downscale_to(&closed, target)
            }
        }
    }
}

/// Blob-gap closing kernel: gaps scale with the symbol's render size, not
/// with module count — `side/divisor` (the min/max filters normalize to an
/// odd window ≥ 3 themselves; ONE normalization site, in `transform`).
fn morph_kernel(luma: &LumaImage, divisor: u32) -> u32 {
    luma.width().max(luma.height()) / divisor.max(1)
}

/// S4 deep rungs, cheap-and-common first. Boosts are the highest-yield
/// class on artistic codes; the morphological closes catch the blob/dot
/// pixel-style class that no contrast transform recovers (two kernel
/// scales cover the observed gap range).
pub(crate) const DEEP_RUNGS: [Rung; 15] = [
    Rung::Boost {
        resize: 400,
        contrast: 2.0,
        brightness: 1.0,
        blur: 0.0,
    },
    Rung::Boost {
        resize: 350,
        contrast: 2.5,
        brightness: 1.0,
        blur: 0.5,
    },
    Rung::Boost {
        resize: 300,
        contrast: 2.0,
        brightness: 1.1,
        blur: 0.3,
    },
    Rung::Boost {
        resize: 400,
        contrast: 1.8,
        brightness: 0.9,
        blur: 0.0,
    },
    Rung::Boost {
        resize: 250,
        contrast: 2.5,
        brightness: 1.0,
        blur: 1.0,
    },
    Rung::Boost {
        resize: 300,
        contrast: 3.0,
        brightness: 1.0,
        blur: 0.8,
    },
    Rung::Boost {
        resize: 0,
        contrast: 2.5,
        brightness: 1.0,
        blur: 0.0,
    },
    Rung::Boost {
        resize: 0,
        contrast: 2.0,
        brightness: 1.1,
        blur: 0.5,
    },
    Rung::Boost {
        resize: 500,
        contrast: 1.5,
        brightness: 1.0,
        blur: 0.0,
    },
    Rung::Boost {
        resize: 450,
        contrast: 2.2,
        brightness: 1.0,
        blur: 0.3,
    },
    Rung::Boost {
        resize: 350,
        contrast: 3.5,
        brightness: 1.2,
        blur: 1.0,
    },
    Rung::Boost {
        resize: 300,
        contrast: 4.0,
        brightness: 1.0,
        blur: 1.5,
    },
    Rung::MorphCloseDark {
        divisor: 93,
        target: 360,
    },
    Rung::MorphCloseDark {
        divisor: 76,
        target: 420,
    },
    Rung::MorphCloseLight {
        divisor: 93,
        target: 360,
    },
];

/// One payload after cross-engine, cross-attempt merging.
#[derive(Debug, Clone)]
pub(crate) struct MergedDetection {
    pub raw: Vec<u8>,
    /// Charset-resolved text — the merge key (engines may disagree on raw
    /// byte representation for kanji-mode: rxing re-encodes text as UTF-8,
    /// rqrr preserves the original Shift-JIS bytes).
    pub text: String,
    pub charset: crate::report::Charset,
    pub masked_stream: Option<MaskedStream>,
    pub corners: Option<[Point; 4]>,
    pub version: Option<u8>,
    pub ec: Option<EcLevel>,
    pub mask: Option<u8>,
    /// FNC1-in-first-position (GS1) — true if ANY contributing engine saw
    /// the FNC1 mode header (only rxing can; rqrr rejects FNC1 symbols).
    pub fnc1: bool,
    pub engines: Vec<EngineKind>,
}

/// Ladder result: merged detections + the execution trace.
#[derive(Debug)]
pub(crate) struct LadderOutcome {
    pub merged: Vec<MergedDetection>,
    pub trace: PipelineTrace,
}

struct Run<'a> {
    cancel: &'a CancelToken,
    deadline: Option<Instant>,
    /// Original input dimensions — every attempt's detections are rescaled
    /// back into this coordinate space (decodes happen on downscales).
    orig: (u32, u32),
    merged: Vec<MergedDetection>,
    panics: u8,
}

impl Run<'_> {
    fn check_cancel(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(ScanError::Cancelled);
        }
        Ok(())
    }

    fn out_of_budget(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    /// Merge one engine pass into the accumulated detections, keyed by the
    /// charset-RESOLVED text (linear scan: N is tiny, order deterministic).
    ///
    /// Raw bytes are NOT the key: for kanji-mode symbols rxing re-encodes
    /// the decoded text as UTF-8 while rqrr preserves the original Shift-JIS
    /// bytes — same symbol, different raw. When a detection carrying the
    /// engine-sampled bitstream (rqrr) joins a merge, its raw REPLACES the
    /// text-derived one: original bytes are the truth.
    fn absorb(&mut self, found: Vec<RawDetection>) -> u32 {
        let count = u32::try_from(found.len()).unwrap_or(u32::MAX);
        for detection in found {
            let (text, charset) = engine::charset::resolve(&detection.raw);
            let slot = self
                .merged
                .iter()
                .position(|existing| existing.text == text);
            match slot {
                Some(index) => {
                    let existing = &mut self.merged[index];
                    if existing.masked_stream.is_none()
                        && let Some(stream) = detection.masked_stream
                    {
                        // bitstream source (rqrr) — its raw is the original
                        // bytes AND its format metadata measured the same
                        // grid: adopt them together (a mixed ec/stream pair
                        // would compute UEC against the wrong RS parameters)
                        existing.masked_stream = Some(stream);
                        existing.raw = detection.raw;
                        existing.charset = charset;
                        existing.version = detection.version.or(existing.version);
                        existing.ec = detection.ec.or(existing.ec);
                        existing.mask = detection.mask.or(existing.mask);
                    } else {
                        existing.version = existing.version.or(detection.version);
                        existing.ec = existing.ec.or(detection.ec);
                        existing.mask = existing.mask.or(detection.mask);
                    }
                    existing.corners = existing.corners.or(detection.corners);
                    existing.fnc1 |= detection.fnc1;
                    if !existing.engines.contains(&detection.engine) {
                        existing.engines.push(detection.engine);
                    }
                }
                None if self.merged.len() >= MAX_DETECTIONS => {}
                None => self.merged.push(MergedDetection {
                    raw: detection.raw,
                    text,
                    charset,
                    masked_stream: detection.masked_stream,
                    corners: detection.corners,
                    version: detection.version,
                    ec: detection.ec,
                    mask: detection.mask,
                    fnc1: detection.fnc1,
                    engines: vec![detection.engine],
                }),
            }
        }
        count
    }

    /// Run one stage as a fixed sequence of lazily-built attempts.
    /// Returns `Ok(true)` when the ladder should stop (budget mid-stage).
    fn stage(
        &mut self,
        name: &str,
        attempts: Vec<Box<dyn Fn() -> LumaImage + '_>>,
        stages: &mut Vec<StageTrace>,
    ) -> Result<bool> {
        let started = Instant::now();
        let mut tried = 0u32;
        let mut found_total = 0u32;
        let mut stop = false;
        for build in attempts {
            self.check_cancel()?;
            if self.out_of_budget() {
                stop = true;
                break;
            }
            let img = build();
            let outcome = engine::decode_all(&img);
            self.panics = self.panics.saturating_add(outcome.panics);
            let rescaled =
                rescale_corners(outcome.detections, (img.width(), img.height()), self.orig);
            found_total += self.absorb(rescaled);
            tried += 1;
        }
        stages.push(StageTrace {
            stage: name.to_owned(),
            transforms_tried: tried,
            ms: started.elapsed().as_secs_f64() * 1_000.0,
            detections_found: found_total,
        });
        Ok(stop)
    }
}

/// S4 attempt list: 12 boost rungs, then the size × contrast × binarization
/// grid in fixed declared order (grid combos duplicating S3 are skipped).
fn deep_attempts(luma: &LumaImage, longest: u32) -> Vec<Box<dyn Fn() -> LumaImage + '_>> {
    let mut attempts: Vec<Box<dyn Fn() -> LumaImage + '_>> = Vec::new();
    for rung in DEEP_RUNGS {
        attempts.push(Box::new(move || rung.apply(luma)));
    }
    for size in [Some(512u32), Some(800), None] {
        if let Some(side) = size
            && side >= longest
        {
            continue;
        }
        for stretch in [false, true] {
            if size.is_none() && !stretch {
                continue; // duplicates S3 otsu/invert at full res
            }
            for op in [transform::otsu_threshold, transform::invert] {
                attempts.push(Box::new(move || {
                    let scaled = match size {
                        Some(side) => transform::downscale_to(luma, side),
                        None => luma.clone(),
                    };
                    let based = if stretch {
                        transform::contrast_stretch(&scaled)
                    } else {
                        scaled
                    };
                    op(&based)
                }));
            }
        }
    }
    attempts
}

/// Map detection corners from the attempt image's coordinate space back to
/// the original input's (decodes run on downscales; consumers — overlays,
/// structural sampling — need original-space geometry).
#[expect(
    clippy::cast_precision_loss,
    reason = "dimensions bounded by Limits::max_dimension — exact in f32"
)]
fn rescale_corners(
    mut found: Vec<RawDetection>,
    attempt_dims: (u32, u32),
    orig_dims: (u32, u32),
) -> Vec<RawDetection> {
    if attempt_dims == orig_dims {
        return found;
    }
    let fx = orig_dims.0 as f32 / attempt_dims.0 as f32;
    let fy = orig_dims.1 as f32 / attempt_dims.1 as f32;
    for detection in &mut found {
        if let Some(corners) = &mut detection.corners {
            for p in corners.iter_mut() {
                p.x *= fx;
                p.y *= fy;
            }
        }
    }
    found
}

/// Execute the ladder over normalized planes. `deadline` is the WHOLE-scan
/// wall-clock bound, shared with the scoring stage (computed by the caller).
pub(crate) fn run(
    planes: &SourcePlanes,
    config: &ScanConfig,
    cancel: &CancelToken,
    deadline: Option<Instant>,
) -> Result<LadderOutcome> {
    let started = Instant::now();
    // Engines never see anything bigger than max_engine_side: one rxing
    // TryHarder pass on a huge image is an unbounded, uninterruptible burn.
    let work = transform::downscale_to(&planes.luma, config.max_engine_side);
    let mut run = Run {
        cancel,
        deadline,
        orig: (planes.luma.width(), planes.luma.height()),
        merged: Vec::new(),
        panics: 0,
    };
    let mut stages = Vec::new();
    let luma = &work;
    let longest = luma.width().max(luma.height());

    'ladder: {
        // S1 — pyramid: a ≤pyramid_side downscale is the cheapest decode AND
        // often the most effective one on artistic codes.
        if config.pyramid && longest > config.pyramid_side && !run.out_of_budget() {
            let side = config.pyramid_side;
            let stop = run.stage(
                "pyramid",
                vec![Box::new(move || transform::downscale_to(luma, side))],
                &mut stages,
            )?;
            if stop || !run.merged.is_empty() {
                break 'ladder;
            }
        }

        // S2 — direct full resolution.
        if config.direct && !run.out_of_budget() {
            let stop = run.stage("direct", vec![Box::new(|| luma.clone())], &mut stages)?;
            if stop || !run.merged.is_empty() {
                break 'ladder;
            }
        }

        // S3 — enhance: fixed transform set at full resolution.
        if config.enhance && !run.out_of_budget() {
            let mut attempts: Vec<Box<dyn Fn() -> LumaImage + '_>> = vec![
                Box::new(|| transform::otsu_threshold(luma)),
                Box::new(|| transform::invert(luma)),
                Box::new(|| transform::contrast_stretch(luma)),
            ];
            for channel in [Channel::R, Channel::G, Channel::B] {
                if let Some(plane) = planes.channel(channel) {
                    // channel planes extract at source resolution — cap them
                    // before they reach the engines
                    let capped = transform::downscale_to(&plane, config.max_engine_side);
                    attempts.push(Box::new(move || capped.clone()));
                }
            }
            let stop = run.stage("enhance", attempts, &mut stages)?;
            if stop || !run.merged.is_empty() {
                break 'ladder;
            }
        }

        // S4 — deep: the curated boost rungs (v0.2 empirical known-good)
        // first, then the size × contrast × binarization grid.
        if config.deep && !run.out_of_budget() {
            let _ = run.stage("deep", deep_attempts(luma, longest), &mut stages)?;
        }
    }

    Ok(LadderOutcome {
        merged: run.merged,
        trace: PipelineTrace {
            stages,
            engine_panics: run.panics,
            total_ms: started.elapsed().as_secs_f64() * 1_000.0,
        },
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::input::{ImageInput, Limits};
    use crate::transform::normalize;

    #[test]
    fn profile_presets_pinned() {
        let full = ScanProfile::Full.config();
        assert!(full.pyramid && full.direct && full.enhance && full.deep);
        assert_eq!(full.budget_ms, Some(4_000));
        assert_eq!(full.pyramid_side, 512);

        let fast = ScanProfile::Fast.config();
        assert!(fast.pyramid && fast.direct && fast.enhance && !fast.deep);
        assert_eq!(fast.budget_ms, Some(800));

        let frame = ScanProfile::Frame.config();
        assert!(frame.pyramid && frame.direct && !frame.enhance && !frame.deep);
        assert_eq!(frame.budget_ms, Some(80));
        assert_eq!(full.score_depth, ScoreDepth::Full);
        assert_eq!(fast.score_depth, ScoreDepth::Reduced);
        assert_eq!(frame.score_depth, ScoreDepth::Off);

        assert_eq!(ScanProfile::default(), ScanProfile::Full);
    }

    #[test]
    fn precancelled_token_short_circuits() {
        let data = vec![255u8; 64 * 64];
        let planes = normalize(&ImageInput::luma8(&data, 64, 64), &Limits::default()).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = run(&planes, &ScanConfig::full(), &cancel, None).unwrap_err();
        assert_eq!(err.code(), "QRS-005");
    }

    #[test]
    fn zero_budget_returns_empty_without_attempts() {
        let data = vec![255u8; 64 * 64];
        let planes = normalize(&ImageInput::luma8(&data, 64, 64), &Limits::default()).unwrap();
        let mut config = ScanConfig::full();
        config.budget_ms = Some(0);
        let deadline = Some(Instant::now());
        let outcome = run(&planes, &config, &CancelToken::new(), deadline).unwrap();
        assert!(outcome.merged.is_empty());
        let attempts: u32 = outcome
            .trace
            .stages
            .iter()
            .map(|s| s.transforms_tried)
            .sum();
        assert_eq!(attempts, 0, "zero budget must not run engine attempts");
    }

    #[test]
    fn white_image_walks_the_whole_ladder_and_stays_empty() {
        let data = vec![255u8; 64 * 64];
        let planes = normalize(&ImageInput::luma8(&data, 64, 64), &Limits::default()).unwrap();
        let mut config = ScanConfig::full();
        config.budget_ms = None;
        let outcome = run(&planes, &config, &CancelToken::new(), None).unwrap();
        assert!(outcome.merged.is_empty());
        // 64px image: pyramid skipped (≤512), direct + enhance + deep ran.
        let names: Vec<&str> = outcome
            .trace
            .stages
            .iter()
            .map(|s| s.stage.as_str())
            .collect();
        assert_eq!(names, vec!["direct", "enhance", "deep"]);
        // luma-only source: no channel attempts in enhance.
        assert_eq!(outcome.trace.stages[1].transforms_tried, 3);
        // deep on a small image: 15 deep rungs (12 boost + 3 morph) +
        // full-res stretch×{otsu,invert}.
        assert_eq!(outcome.trace.stages[2].transforms_tried, 17);
    }

    #[test]
    fn profile_wire_names_round_trip() {
        // THE cross-language name mapping — bindings and CLI all parse
        // through here (drift in any arm breaks a published flag).
        assert_eq!(ScanProfile::from_name("full"), Some(ScanProfile::Full));
        assert_eq!(ScanProfile::from_name("fast"), Some(ScanProfile::Fast));
        assert_eq!(ScanProfile::from_name("frame"), Some(ScanProfile::Frame));
        assert_eq!(ScanProfile::from_name("FULL"), None);
        assert_eq!(ScanProfile::from_name(""), None);
    }

    #[test]
    fn boost_rung_resize_zero_keeps_native_resolution() {
        // resize: 0 means "work at native res" — NOT a 0-px downscale.
        let img = LumaImage::new(vec![128u8; 64 * 32], 64, 32);
        let rung = Rung::Boost {
            resize: 0,
            contrast: 2.5,
            brightness: 1.0,
            blur: 0.0,
        };
        let out = rung.apply(&img);
        assert_eq!((out.width(), out.height()), (64, 32));
    }

    #[test]
    fn morph_rungs_close_gaps_and_hit_their_target_size() {
        // checkerboard of 2px dark dots with 2px gaps — a close with a
        // big-enough kernel turns the dark quadrant solid black.
        let side = 840u32;
        let mut data = vec![255u8; (side * side) as usize];
        for y in 0..side {
            for x in 0..side {
                if (x / 2 + y / 2) % 2 == 0 {
                    data[(y * side + x) as usize] = 0;
                }
            }
        }
        let img = LumaImage::new(data, side, side);
        let rung = Rung::MorphCloseDark {
            divisor: 93, // k = 840/93 = 9 ≥ gap span
            target: 360,
        };
        let out = rung.apply(&img);
        assert_eq!((out.width(), out.height()), (360, 360));
        assert!(
            out.data().iter().all(|&p| p == 0),
            "close must merge the dot grid into solid dark"
        );
    }
}

#[cfg(test)]
mod probe {
    //! Dev diagnostics — `cargo nextest run probe_ --run-ignored all --no-capture`.
    //! Sweeps the transform space over a corpus image to inform ladder order.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::input::{ImageInput, Limits};
    use crate::transform::normalize;

    #[test]
    #[ignore = "dev diagnostic, not a contract"]
    fn probe_artistic_transform_space() {
        let path = format!(
            "{}/../../fixtures/artistic/OK_1069ms_85_8b6a54b3.png",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(path).unwrap();
        let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
        let luma = &planes.luma;
        println!("image {}x{}", luma.width(), luma.height());

        let sizes: [Option<u32>; 5] = [None, Some(800), Some(512), Some(384), Some(256)];
        let mut planes_set: Vec<(String, LumaImage)> = vec![("luma".into(), luma.clone())];
        for channel in [Channel::R, Channel::G, Channel::B] {
            if let Some(p) = planes.channel(channel) {
                planes_set.push((format!("{channel:?}"), p));
            }
        }
        for (plane_name, plane) in &planes_set {
            for size in sizes {
                let scaled = match size {
                    Some(s) => transform::downscale_to(plane, s),
                    None => plane.clone(),
                };
                let variants: Vec<(&str, LumaImage)> = vec![
                    ("plain", scaled.clone()),
                    ("otsu", transform::otsu_threshold(&scaled)),
                    ("invert", transform::invert(&scaled)),
                    ("stretch", transform::contrast_stretch(&scaled)),
                    (
                        "stretch+otsu",
                        transform::otsu_threshold(&transform::contrast_stretch(&scaled)),
                    ),
                ];
                for (op_name, img) in variants {
                    let outcome = engine::decode_all(&img);
                    if !outcome.detections.is_empty() {
                        let engines: Vec<_> = outcome.detections.iter().map(|d| d.engine).collect();
                        println!(
                            "HIT plane={plane_name} size={size:?} op={op_name} engines={engines:?}"
                        );
                    }
                }
            }
        }
        println!("probe done");
    }

    #[test]
    #[ignore = "dev diagnostic, not a contract"]
    fn probe_artistic_v02_known_good_combos() {
        let path = format!(
            "{}/../../fixtures/artistic/OK_1069ms_85_8b6a54b3.png",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(path).unwrap();
        let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
        let luma = &planes.luma;

        // THE canonical rungs — the probe must never drift from the ladder.
        for rung in DEEP_RUNGS {
            let img = rung.apply(luma);
            let outcome = engine::decode_all(&img);
            if !outcome.detections.is_empty() {
                let engines: Vec<_> = outcome.detections.iter().map(|d| d.engine).collect();
                println!("HIT rung={rung:?} engines={engines:?}");
            }
        }
        println!("v02 probe done");
    }

    #[test]
    #[ignore = "dev diagnostic, not a contract"]
    fn probe_monkey_morph_variants() {
        let path = format!(
            "{}/../../fixtures/artistic/blob-style-monkey-logo.webp",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(path).unwrap();
        let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
        let luma = &planes.luma;
        println!("luma {}x{}", luma.width(), luma.height());
        for k in [5u32, 7, 9, 11, 13] {
            for target in [256u32, 290, 320, 360, 420, 512] {
                let closed = transform::min_filter(luma, k);
                let img = transform::downscale_to(&closed, target);
                let outcome = engine::decode_all(&img);
                if !outcome.detections.is_empty() {
                    println!(
                        "HIT k={k} target={target} engines={:?}",
                        outcome
                            .detections
                            .iter()
                            .map(|d| d.engine)
                            .collect::<Vec<_>>()
                    );
                }
            }
        }
        println!("probe done");
    }
}

#[cfg(test)]
mod kanji_merge_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::input::{ImageInput, Limits};
    use crate::transform::normalize;

    /// Kanji-MODE divergence: rqrr returns the original Shift-JIS bytes,
    /// rxing (no `BYTE_SEGMENTS` for kanji segments) returns UTF-8 of the
    /// decoded text. Same symbol MUST merge into one detection, raw = the
    /// original SJIS bytes (rqrr is the byte-truth engine).
    #[test]
    fn kanji_mode_merges_across_engines_with_sjis_raw() {
        // こんにちは in Shift-JIS — the qrcode crate's optimizer emits
        // kanji-mode segments for SJIS pairs.
        let sjis: &[u8] = &[0x82, 0xB1, 0x82, 0xF1, 0x82, 0xC9, 0x82, 0xBF, 0x82, 0xCD];
        let code = qrcode::QrCode::with_error_correction_level(sjis, qrcode::EcLevel::Q).unwrap();
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(6, 6)
            .build();
        let mut png = Vec::new();
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();

        let planes = normalize(&ImageInput::encoded(&png), &Limits::default()).unwrap();
        let outcome = run(&planes, &ScanConfig::full(), &CancelToken::new(), None).unwrap();

        assert_eq!(
            outcome.merged.len(),
            1,
            "one symbol must merge to ONE detection — got {:?}",
            outcome
                .merged
                .iter()
                .map(|m| (&m.engines, &m.raw))
                .collect::<Vec<_>>()
        );
        let d = &outcome.merged[0];
        assert_eq!(d.raw, sjis, "raw must be the ORIGINAL bytes (rqrr truth)");
        let (text, charset) = crate::engine::charset::resolve(&d.raw);
        assert_eq!(text, "こんにちは");
        assert_eq!(charset, crate::report::Charset::ShiftJis);
    }
}
