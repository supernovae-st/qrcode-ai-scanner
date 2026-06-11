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
use std::time::Duration;

use web_time::Instant;

use crate::engine::{self, EngineOptions, MaskedStream, RawDetection};
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
    pub(crate) fn config(self) -> ScanConfig {
        match self {
            Self::Full => ScanConfig::full(),
            Self::Fast => ScanConfig::fast(),
            Self::Frame => ScanConfig::frame(),
            Self::Custom(config) => config,
        }
    }
}

/// S4 boost rungs `(resize, contrast, brightness, blur)` — the v0.2 tier-3
/// "known-good" set, empirically selected on the original 74-image artistic
/// corpus. Small resize + multiplicative contrast (+ light blur) averages art
/// texture into module means — the single highest-yield class on artistic
/// codes (probe-verified on the legacy corpus, 2026-06-11).
const BOOST_RUNGS: [(u32, f32, f32, f32); 12] = [
    (400, 2.0, 1.0, 0.0),
    (350, 2.5, 1.0, 0.5),
    (300, 2.0, 1.1, 0.3),
    (400, 1.8, 0.9, 0.0),
    (250, 2.5, 1.0, 1.0),
    (300, 3.0, 1.0, 0.8),
    (0, 2.5, 1.0, 0.0),
    (0, 2.0, 1.1, 0.5),
    (500, 1.5, 1.0, 0.0),
    (450, 2.2, 1.0, 0.3),
    (350, 3.5, 1.2, 1.0),
    (300, 4.0, 1.0, 1.5),
];

/// Build one boost rung image: downscale → contrast/brightness boost → blur.
fn boost_rung(luma: &LumaImage, rung: (u32, f32, f32, f32)) -> LumaImage {
    let (resize, contrast, brightness, blur) = rung;
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

/// One payload after cross-engine, cross-attempt merging.
#[derive(Debug, Clone)]
pub(crate) struct MergedDetection {
    pub raw: Vec<u8>,
    pub masked_stream: Option<MaskedStream>,
    pub corners: Option<[Point; 4]>,
    pub version: Option<u8>,
    pub ec: Option<EcLevel>,
    pub mask: Option<u8>,
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

    /// Merge one engine pass into the accumulated detections, keyed by raw
    /// payload bytes (linear scan: N is tiny, order stays deterministic).
    fn absorb(&mut self, found: Vec<RawDetection>) -> u32 {
        let count = u32::try_from(found.len()).unwrap_or(u32::MAX);
        for detection in found {
            match self
                .merged
                .iter_mut()
                .find(|existing| existing.raw == detection.raw)
            {
                Some(existing) => {
                    if existing.masked_stream.is_none() {
                        existing.masked_stream = detection.masked_stream;
                    }
                    existing.corners = existing.corners.or(detection.corners);
                    existing.version = existing.version.or(detection.version);
                    existing.ec = existing.ec.or(detection.ec);
                    existing.mask = existing.mask.or(detection.mask);
                    if !existing.engines.contains(&detection.engine) {
                        existing.engines.push(detection.engine);
                    }
                }
                None => self.merged.push(MergedDetection {
                    raw: detection.raw,
                    masked_stream: detection.masked_stream,
                    corners: detection.corners,
                    version: detection.version,
                    ec: detection.ec,
                    mask: detection.mask,
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
            let outcome = engine::decode_all(&img, EngineOptions::default());
            self.panics = self.panics.saturating_add(outcome.panics);
            found_total += self.absorb(outcome.detections);
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

/// Execute the ladder over normalized planes.
pub(crate) fn run(
    planes: &SourcePlanes,
    config: &ScanConfig,
    cancel: &CancelToken,
) -> Result<LadderOutcome> {
    let started = Instant::now();
    let mut run = Run {
        cancel,
        deadline: config
            .budget_ms
            .map(|ms| started + Duration::from_millis(ms)),
        merged: Vec::new(),
        panics: 0,
    };
    let mut stages = Vec::new();
    let luma = &planes.luma;
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
                    attempts.push(Box::new(move || plane.clone()));
                }
            }
            let stop = run.stage("enhance", attempts, &mut stages)?;
            if stop || !run.merged.is_empty() {
                break 'ladder;
            }
        }

        // S4 — deep: the curated boost rungs (v0.2 empirical known-good)
        // first, then the size × contrast × binarization grid. Fixed declared
        // order; grid combinations that duplicate S3 attempts are skipped.
        if config.deep && !run.out_of_budget() {
            let mut attempts: Vec<Box<dyn Fn() -> LumaImage + '_>> = Vec::new();
            for rung in BOOST_RUNGS {
                attempts.push(Box::new(move || boost_rung(luma, rung)));
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
            let _ = run.stage("deep", attempts, &mut stages)?;
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
        let err = run(&planes, &ScanConfig::full(), &cancel).unwrap_err();
        assert_eq!(err.code(), "QRS-005");
    }

    #[test]
    fn zero_budget_returns_empty_without_attempts() {
        let data = vec![255u8; 64 * 64];
        let planes = normalize(&ImageInput::luma8(&data, 64, 64), &Limits::default()).unwrap();
        let mut config = ScanConfig::full();
        config.budget_ms = Some(0);
        let outcome = run(&planes, &config, &CancelToken::new()).unwrap();
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
        let outcome = run(&planes, &config, &CancelToken::new()).unwrap();
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
        // deep on a small image: 12 boost rungs + full-res stretch×{otsu,invert}.
        assert_eq!(outcome.trace.stages[2].transforms_tried, 14);
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
            "{}/../../test-images/artistic/OK_1069ms_85_8b6a54b3.png",
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
                    let outcome = engine::decode_all(&img, EngineOptions::default());
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
            "{}/../../test-images/artistic/OK_1069ms_85_8b6a54b3.png",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(path).unwrap();
        let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
        let luma = &planes.luma;

        // (resize, contrast, brightness, blur) — the 12 v0.2 tier-3 rows.
        let combos: [(u32, f32, f32, f32); 12] = [
            (400, 2.0, 1.0, 0.0),
            (350, 2.5, 1.0, 0.5),
            (300, 2.0, 1.1, 0.3),
            (400, 1.8, 0.9, 0.0),
            (250, 2.5, 1.0, 1.0),
            (300, 3.0, 1.0, 0.8),
            (0, 2.5, 1.0, 0.0),
            (0, 2.0, 1.1, 0.5),
            (500, 1.5, 1.0, 0.0),
            (450, 2.2, 1.0, 0.3),
            (350, 3.5, 1.2, 1.0),
            (300, 4.0, 1.0, 1.5),
        ];
        for (resize, contrast, brightness, blur) in combos {
            let sized = if resize > 0 {
                transform::downscale_to(luma, resize)
            } else {
                luma.clone()
            };
            let boosted = transform::contrast_boost(&sized, contrast, brightness);
            let img = if blur > 0.3 {
                transform::gaussian_blur(&boosted, blur)
            } else {
                boosted
            };
            let outcome = engine::decode_all(&img, EngineOptions::default());
            if !outcome.detections.is_empty() {
                let engines: Vec<_> = outcome.detections.iter().map(|d| d.engine).collect();
                println!(
                    "HIT resize={resize} contrast={contrast} brightness={brightness} blur={blur} engines={engines:?}"
                );
            }
        }
        println!("v02 probe done");
    }
}
