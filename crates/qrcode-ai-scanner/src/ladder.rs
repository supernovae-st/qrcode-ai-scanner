//! Deterministic decode ladder — S1 pyramid → S2 direct → S3 enhance → S4
//! deep → S5 rescue.
//!
//! Replaces the v0.2 RNG brute force: the attempt sequence is a fixed,
//! declared order, so the same input under the same config always walks the
//! same path. Budget (wall clock, attempt-granular — an engine call is not
//! interruptible) and cooperative cancellation are checked between attempts.
//! Early exit happens at stage boundaries: a stage that finds something
//! still completes, so consensus data within that stage is preserved — and
//! a stage that found only QR Code reads whose ECI declarations are
//! signalled, never located, does not end the walk (a later stage may
//! locate them).
//!
//! Every attempt runs the engines, re-derives the grids rqrr detected but
//! could not decode (at most `MAX_RESCUE_CANDIDATES`, while a QR Code read
//! the scanner has not parsed is in play — rqrr's adapter already decided
//! the first four grids' orientation: per grid at most one extra raw read
//! of its own grid and two plain RS passes, no image sampling), records the
//! refusals they prove, re-reads the reads no parse of the attempt carries
//! where they lie and no recorded refusal withholds (`engine::locate`, at
//! most `MAX_REREADS` per scan) and collects the reads
//! (`engine::reconcile`). Nothing is decided per attempt: the walk ends
//! with one more pass over the rescue candidates collected in every
//! attempt — clean RS, no sampling, past the deadline too (a refusal stays a
//! refusal; cancellation is checked before it) — and the reconciliation of
//! the whole read set.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use web_time::Instant;

use crate::engine::content::{self, ContentKey, EciEvidence, Fnc1, Reading, Segments};
use crate::engine::locate::{LocateHint, Located};
use crate::engine::reconcile::{Geometry, Rank, Read, ReadStore, Reconciled, Seed, inside, source};
use crate::engine::{self, RawDetection, Refusal};
use crate::error::{Result, ScanError};
use crate::input::LumaImage;
use crate::report::{EngineKind, PipelineTrace, Point, StageTrace, Symbology};
use crate::rescue::{Acceptance, Rederived, RescueCandidate};
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
/// only constructors). Clone-not-Copy: the axis-skip list is a collection.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
#[expect(
    clippy::struct_excessive_bools,
    reason = "stage switches ARE independent booleans — a bitflags layer would obscure the API"
)]
pub struct ScanConfig {
    /// Wall-clock budget in milliseconds. `None` = unbounded: the scan runs the full
    /// ladder, bounded only by the input-size `Limits`, never by time. Every built-in
    /// profile sets one (full 4 s · fast 800 ms · frame 80 ms) and server callers
    /// should keep a budget; leave it `None` only for offline/batch decoding where
    /// reading every symbol outweighs latency. (`Some(0)` cuts immediately; bindings
    /// exposing a numeric budget may map `0` → unbounded — see each binding's docs.)
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
    /// Stress axes EXCLUDED from scoring — integration config for hosts
    /// where an axis is meaningless (a generated preview has no capture
    /// angle: builders skip `perspective` + `rotation`). Skipped axes never
    /// run (their stress cells are never built — real wall-clock savings)
    /// and the composite renormalizes over the axes that DID run, so the
    /// value stays 0-100 on the same contract-v3 weights. The report
    /// self-describes: `score.axes` carries only the axes that ran. Empty
    /// (every profile's default) = the full six-axis contract, byte for
    /// byte. Skipping ALL axes yields no score at all (an axis-less value
    /// would be fiction) — same outcome as `ScoreDepth::Off`.
    pub score_skip_axes: Vec<crate::report::StressAxis>,
    /// Score SECTIONS excluded from the report — the same integration seam
    /// as `score_skip_axes`, for report blocks instead of stress axes. A
    /// host that displays neither the correction margin nor the ISO
    /// parameters skips `uec` + `iso15415`: the sections are never computed
    /// (no UEC bitstream walk · no ISO parameter sweep), the wire carries
    /// `null`, and the UEC-driven hints never fire. The composite
    /// `score.value` is axis-based and does NOT move — skipping checks is
    /// surface truth, never score surgery. Empty (every profile's default)
    /// = every section runs, byte for byte.
    pub score_skip_checks: Vec<ScoreCheck>,
    /// Background handling for inputs that carry transparent pixels — the
    /// flatten runs BEFORE luma conversion and RGB extraction, so every
    /// downstream stage (ladder · score · UEC · ISO) sees the composited
    /// image. Opaque inputs never enter this path: their reports stay
    /// byte-identical whatever this is set to.
    pub alpha_background: AlphaBackground,
    /// HOST palette colors probed by the placement envelope, on top of the
    /// neutral sweep — "does my design survive on MY theme/brand colors",
    /// answered inside one scan instead of one full scan per color
    /// (`alpha.envelope.palette`). Probes are luma-space like the rest of
    /// the sweep and budget-bounded per probe. Every BINDING rejects more
    /// than [`ScanConfig::MAX_ALPHA_PALETTE`] colors loudly (a palette is
    /// the first caller-sized loop in the scan path — unbounded, it would
    /// be a CPU amplifier under `budget_ms: None`); direct Rust embedders
    /// hold the same line themselves. Empty (every profile's default) =
    /// the neutral sweep only.
    pub alpha_palette: Vec<[u8; 3]>,
}

/// A skippable score SECTION — the same integration seam as
/// [`crate::report::StressAxis`]-skipping, for report blocks instead of stress axes. A host
/// that displays neither the correction margin nor the ISO parameters skips
/// them at scan time: the sections are never computed, the wire carries
/// `null`, and the UEC-driven hints never fire. Config-only (never
/// serialized) — the report self-describes through the absent sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreCheck {
    /// Unused-error-correction margin (`score.uec`) — skipping it also
    /// silences the two hints it drives (the thin-margin priority path of
    /// `raise_error_correction`, and `low_correction_margin`).
    Uec,
    /// ISO 15415-informed parameters (`score.iso15415`). Independent of
    /// [`ScoreCheck::Uec`]: skipping UEC alone nulls only the
    /// `unused_error_correction` parameter inside a still-present block.
    Iso15415,
    /// The alpha placement envelope (`alpha.envelope`) — the neutral-
    /// background decode sweep for transparent inputs. Skipping it never
    /// touches the flatten itself (the verdict keeps its declared
    /// background); it only drops the sweep and silences the
    /// `alpha_background_dependent` hint.
    AlphaEnvelope,
}

impl ScoreCheck {
    /// Parse the wire name of the section (`uec` / `iso15415` /
    /// `alpha_envelope` — the report field spellings). Same shape as
    /// [`crate::report::StressAxis::from_name`]: `None` on anything else,
    /// so bindings can fail LOUD on a typo.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "uec" => Some(Self::Uec),
            "iso15415" => Some(Self::Iso15415),
            "alpha_envelope" => Some(Self::AlphaEnvelope),
            _ => None,
        }
    }
}

/// Named axis-skip presets — the drift-proof spelling of the two canonical
/// integration postures, so N host surfaces cannot slowly diverge on their
/// hand-built skip lists. Pure sugar over `score_skip_axes` (the bindings
/// reject passing BOTH, loudly): `design` = a generated preview with no
/// capture geometry (skips perspective + rotation · keeps lighting — the
/// glare cell measures the DESIGN's fragility, not the capture) · `capture`
/// = the full six-axis contract for photo/camera surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScorePreset {
    /// Generated-preview posture: skip `perspective` + `rotation`.
    Design,
    /// Photo/camera posture: every axis runs (an explicit spelling of the
    /// default — so integrations can WRITE their posture down).
    Capture,
}

impl ScorePreset {
    /// Parse the cross-language wire name (`design` · `capture`) — the ONE
    /// mapping every binding reuses. `None` on anything else, loud upstream.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "design" => Some(Self::Design),
            "capture" => Some(Self::Capture),
            _ => None,
        }
    }

    /// The axes this preset SKIPS.
    #[must_use]
    pub fn skips(self) -> Vec<crate::report::StressAxis> {
        match self {
            Self::Design => vec![
                crate::report::StressAxis::Perspective,
                crate::report::StressAxis::Rotation,
            ],
            Self::Capture => Vec::new(),
        }
    }
}

/// Background applied under transparent pixels BEFORE luma conversion and
/// RGB extraction — the config side of the report's `alpha` block. A
/// transparent asset has no one background: 0.8.x read the STORED RGB
/// under the transparency, an exporter-dependent verdict (canvas exports
/// store black under full transparency, image editors often white — the
/// same visual design scored 100, 74 or "no detection" depending on which
/// tool exported it). Config-only, never serialized; the report echoes the
/// requested mode and the resolved background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlphaBackground {
    /// Resolve the maximum-contrast background from the design's own
    /// visible pixels: dark content (alpha-weighted mean luma < 128)
    /// flattens over white, light content over black — the code is
    /// measured on its intended placement. A zero-detection scan retries
    /// once over the opposite background (`alpha.fallback_used`), so a
    /// mis-called mean can never turn a decodable design into a false
    /// "no detection".
    Auto,
    /// Force a white background.
    White,
    /// Force a black background.
    Black,
    /// Force the host's real placement background (sRGB) — the truest
    /// verdict when the embedder knows the surface the code will sit on.
    Custom([u8; 3]),
    /// Drop the alpha channel (the pre-0.9 behavior) and carry no `alpha`
    /// block at all. Escape hatch for byte-parity with 0.8.x — never the
    /// default, because the verdict depends on invisible stored RGB.
    None,
}

impl AlphaBackground {
    /// Parse one PALETTE color for the envelope's palette probes —
    /// `white` · `black` · `#rrggbb`. The flatten keywords `auto`/`none`
    /// are modes, not colors, and are rejected here. Same loud-typo seam
    /// as [`AlphaBackground::from_name`].
    #[must_use]
    pub fn palette_color(name: &str) -> Option<[u8; 3]> {
        match name {
            "white" => Some([255; 3]),
            "black" => Some([0; 3]),
            _ => match Self::from_name(name) {
                Some(Self::Custom(rgb)) => Some(rgb),
                _ => None,
            },
        }
    }

    /// Parse the cross-language wire name (`auto` · `white` · `black` ·
    /// `none` · `#rrggbb`) — the ONE mapping every binding reuses. `None`
    /// on anything else, so bindings fail LOUD on a typo.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "auto" => Some(Self::Auto),
            "white" => Some(Self::White),
            "black" => Some(Self::Black),
            "none" => Some(Self::None),
            _ => {
                let hex = name.strip_prefix('#')?;
                // from_str_radix alone would admit signs ("+a") — hexdigit-gate first.
                if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return None;
                }
                let channel = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
                Some(Self::Custom([channel(0)?, channel(2)?, channel(4)?]))
            }
        }
    }
}

impl ScanConfig {
    /// Hard cap every binding enforces on `alpha_palette` (anti-DoS: a
    /// palette entry costs a composite + up to three decode attempts).
    /// 32 covers any real theme/brand palette with headroom.
    pub const MAX_ALPHA_PALETTE: usize = 32;

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
            score_skip_axes: Vec::new(),
            score_skip_checks: Vec::new(),
            alpha_background: AlphaBackground::Auto,
            alpha_palette: Vec::new(),
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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

    /// The profile's [`ScanConfig`] — the embedder hook for adjusting a
    /// preset (e.g. tightening `budget_ms` for a UI-thread scan) before
    /// wrapping it back in [`ScanProfile::Custom`].
    #[must_use]
    pub fn config(self) -> ScanConfig {
        match self {
            Self::Full => ScanConfig::full(),
            Self::Fast => ScanConfig::fast(),
            Self::Frame => ScanConfig::frame(),
            Self::Custom(config) => config,
        }
    }
}

/// Rescue candidates kept per walk (S5 cost is per candidate) and
/// re-derived per attempt.
const MAX_RESCUE_CANDIDATES: usize = crate::rescue::MAX_CANDIDATES;

/// Re-reads per scan (both walks of an alpha fallback together): each
/// spends at most `engine::locate::MAX_PROBES` format probes and as many
/// error-correction attempts.
const MAX_REREADS: usize = 16;

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

/// One published content: the reconciled reads and the scanner's one
/// reading of them (its field set is what the score evaluates).
#[derive(Debug, Clone)]
pub(crate) struct MergedDetection {
    pub symbology: crate::report::Symbology,
    /// Original payload bytes, designators excluded.
    pub raw: Vec<u8>,
    /// The scanner's one interpretation of the content (`engine::content`).
    /// Identity is the content, not this text: two contents that read the
    /// same stay two detections.
    pub text: String,
    /// How `text` relates to `raw` (spec/01 § Content).
    pub charset: crate::report::Charset,
    pub masked_stream: Option<engine::MaskedStream>,
    pub corners: Option<[Point; 4]>,
    pub version: Option<u8>,
    pub ec: Option<crate::report::EcLevel>,
    pub mask: Option<u8>,
    /// FNC1 in first position (GS1). Second position is never GS1: the
    /// position itself travels in [`LadderOutcome::contents`].
    pub fnc1: bool,
    /// The GEOMETRY SOURCE's attempt photometrically inverted the symbol
    /// (light-on-dark original). Meaningful only when `corners` is `Some`;
    /// adopted together with the corners.
    pub photometric_inverted: bool,
    pub engines: Vec<EngineKind>,
}

/// Ladder result: the published detections, what they are, the walk's
/// reads and refusals, and the execution trace.
#[derive(Debug)]
pub(crate) struct LadderOutcome {
    pub merged: Vec<MergedDetection>,
    /// Each detection's content and its one reading, aligned with `merged`:
    /// what the wire publishes (`eci`, `fnc1`) and what probes match reads
    /// against (`ContentKey::admits`).
    pub contents: Vec<(ContentKey, Reading)>,
    /// The walk's reads and refusals.
    store: ReadStore,
    /// Re-reads the scan spent.
    rereads: usize,
    pub trace: PipelineTrace,
}

impl LadderOutcome {
    /// What the opposite-background walk starts from: this walk's
    /// refusals (those past capacity included), the contents its parses
    /// read, and the re-reads already spent.
    pub(crate) fn seed(&self) -> Seed {
        self.store.seed(self.rereads)
    }

    /// Fold a second walk (the alpha fallback) in: both walks stay in the
    /// trace, its detections replace these only when it found some. It
    /// started from this walk's refusals and protection ([`run_seeded`]) and
    /// applied them place by place, so whatever it publishes already stands
    /// against them. Returns whether the fallback found a detection.
    pub(crate) fn fold_fallback(&mut self, fallback: LadderOutcome) -> bool {
        self.trace.stages.extend(fallback.trace.stages);
        self.trace.engine_panics = self
            .trace
            .engine_panics
            .saturating_add(fallback.trace.engine_panics);
        self.trace.total_ms += fallback.trace.total_ms;
        let found = !fallback.merged.is_empty();
        if found {
            self.merged = fallback.merged;
            self.contents = fallback.contents;
        }
        found
    }
}

struct Run<'a> {
    cancel: &'a CancelToken,
    deadline: Option<Instant>,
    /// Original input dimensions — every attempt's geometry is rescaled
    /// back into this coordinate space (decodes happen on downscales).
    orig: (u32, u32),
    /// The walk's reads and refusals.
    store: ReadStore,
    /// Grids detected but not decoded (rqrr stream readable) — S5 inputs
    /// and the end-of-walk pass's grids. Corners already rescaled to
    /// original space; capped to bound cost.
    rescue_candidates: Vec<RescueCandidate>,
    /// Attempts made so far: the next attempt's rank.
    attempts: u32,
    /// Re-reads the scan spent.
    rereads: usize,
    panics: u8,
}

/// The candidate geometry: its grid's corners with the photometry of its
/// attempt, and the stream with the format read from it.
fn candidate_geometry(candidate: &RescueCandidate) -> Geometry {
    Geometry {
        corners: Some((candidate.corners, candidate.inverted)),
        stream: Some((
            candidate.stream.clone(),
            candidate.version,
            candidate.ec,
            candidate.mask,
        )),
        ec: None,
    }
}

/// An engine read the scanner has not parsed, of a QR Code symbol.
fn unparsed_qr(read: &Read) -> bool {
    !read.engines.is_empty() && read.symbology == Symbology::QrCode && !read.parsed
}

impl<'a> Run<'a> {
    fn new(
        cancel: &'a CancelToken,
        deadline: Option<Instant>,
        orig: (u32, u32),
        seed: Seed,
    ) -> Self {
        let rereads = seed.rereads;
        Self {
            cancel,
            deadline,
            orig,
            store: ReadStore::seeded(seed),
            rescue_candidates: Vec::new(),
            attempts: 0,
            rereads,
            panics: 0,
        }
    }
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

    /// The next attempt's rank.
    fn next_attempt(&mut self) -> u32 {
        let attempt = self.attempts;
        self.attempts = self.attempts.saturating_add(1);
        attempt
    }

    /// An engine hit as a read of this walk: original-space place and
    /// geometry, the photometry of its attempt travelling with its
    /// corners; and what its re-read needs (attempt space).
    fn read_of(
        &self,
        hit: RawDetection,
        rank: Rank,
        dims: (u32, u32),
        inverts: bool,
    ) -> (Read, Option<LocateHint>) {
        let place = |corners: [Point; 4]| rescaled(corners, dims, self.orig);
        let stream = match (hit.masked_stream, hit.version, hit.ec, hit.mask) {
            (Some(stream), Some(version), Some(ec), Some(mask)) => {
                Some((stream, version, ec, mask))
            }
            _ => None,
        };
        let geometry = Geometry {
            corners: hit.corners.map(|corners| (place(corners), inverts)),
            ec: if stream.is_none() { hit.ec } else { None },
            stream,
        };
        let key = ContentKey {
            raw: hit.raw,
            eci: hit.eci,
            fnc1: hit.fnc1,
        };
        let mut read =
            Read::new(hit.symbology, key, Some(hit.engine), rank).at(hit.location.map(place));
        read.segments = hit.segments;
        read.engine_reading = hit.engine_reading;
        read.parsed = hit.parsed;
        read.geometry = geometry;
        (read, hit.locate)
    }

    /// Re-derive grids rqrr detected but did not decode (plain RS, no luma
    /// sampling — or the re-derivation the engine pass already chose their
    /// orientation with): a stream that parses is a candidate parse — its
    /// declarations, FNC1 position, segments and geometry, no engine; one
    /// proven malformed is a refusal at the grid's place. At most
    /// `MAX_RESCUE_CANDIDATES`, in the order given (original-space corners,
    /// photometry stamped).
    fn rederive(candidates: &[RescueCandidate], attempt: u32) -> (Vec<Read>, Vec<Refusal>) {
        let mut reads = Vec::new();
        let mut refusals = Vec::new();
        for (index, candidate) in candidates.iter().take(MAX_RESCUE_CANDIDATES).enumerate() {
            let derived = candidate.rederived.clone().unwrap_or_else(|| {
                crate::rescue::rederive(
                    &candidate.stream,
                    candidate.version,
                    candidate.ec,
                    candidate.mask,
                    Acceptance::Engine,
                )
            });
            match derived {
                Rederived::Parsed(parsed) => {
                    let key = ContentKey {
                        raw: parsed.bytes,
                        eci: EciEvidence::from_marks(parsed.eci),
                        fnc1: parsed.fnc1,
                    };
                    let rank = Rank {
                        attempt,
                        source: source::CANDIDATE,
                        index: u32::try_from(index).unwrap_or(u32::MAX),
                    };
                    let mut read =
                        Read::new(Symbology::QrCode, key, None, rank).at(Some(candidate.corners));
                    read.segments = Segments::Observed(parsed.segments);
                    read.parsed = true;
                    read.geometry = candidate_geometry(candidate);
                    reads.push(read);
                }
                Rederived::Malformed { port } => {
                    refusals.extend(Refusal::new(port, None, Some(candidate.corners)));
                }
                Rederived::Unverified => {}
            }
        }
        (reads, refusals)
    }

    /// The re-read (07-pipeline property 6): each read with a hint is
    /// sampled at its engine's corners in the attempt image, while the scan
    /// has re-reads left — unless a parsed read or candidate parse of the
    /// attempt carries it where it lies, or a recorded refusal withholds it
    /// (it is withheld anyway). A located parse upgrades it (declarations,
    /// segments — never its bytes, engines or geometry); a refused one
    /// removes it and records the refusal at its place.
    fn reread(
        &mut self,
        img: &LumaImage,
        reads: &mut Vec<(Read, Option<LocateHint>)>,
        refusals: &mut Vec<Refusal>,
    ) {
        let mut i = 0;
        while let Some((read, hint)) = reads.get(i) {
            let carried = reads.iter().any(|(other, _)| {
                other.parsed
                    && other.symbology == read.symbology
                    && other.key.agrees(&read.key)
                    && inside(read.location, other.location)
            });
            let Some(hint) = hint
                .clone()
                .filter(|_| !carried && self.rereads < MAX_REREADS && !self.store.withheld(read))
            else {
                i += 1;
                continue;
            };
            self.rereads += 1;
            match engine::reread(img, &hint, &read.key.raw) {
                Located::Parsed {
                    eci,
                    fnc1,
                    segments,
                    ..
                } => {
                    if let Some((read, _)) = reads.get_mut(i) {
                        read.key.eci = EciEvidence::from_marks(eci);
                        read.key.fnc1 = fnc1;
                        read.segments = Segments::Observed(segments);
                        read.parsed = true;
                    }
                    i += 1;
                }
                Located::Refused { port } => {
                    let (read, _) = reads.remove(i);
                    refusals.push(Refusal {
                        port: Some(engine::port_key(port)),
                        rqrr: None,
                        place: read.location,
                    });
                }
                Located::Unverified => i += 1,
            }
        }
    }

    /// S5 — try the collected rescue candidates when the walk published
    /// nothing (refused and withheld reads do not count); the first success
    /// is collected.
    fn rescue_stage(&mut self, luma: &LumaImage, stages: &mut Vec<StageTrace>) -> Result<()> {
        if self.rescue_candidates.is_empty() || self.out_of_budget() || self.store.found() {
            return Ok(());
        }
        let started = Instant::now();
        let attempt = self.next_attempt();
        let mut tried = 0u32;
        let mut found = 0u32;
        for candidate in self.rescue_candidates.clone() {
            self.check_cancel()?;
            if self.out_of_budget() {
                break;
            }
            tried += 1;
            // Reports retain original-space corners. Rescue samples the
            // capped working plane, including its bounded inverted view.
            let mut sampling = candidate.clone();
            if (luma.width(), luma.height()) != self.orig {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "dimensions bounded by Limits::max_dimension — exact in f32"
                )]
                let (fx, fy) = (
                    luma.width() as f32 / self.orig.0 as f32,
                    luma.height() as f32 / self.orig.1 as f32,
                );
                for corner in &mut sampling.corners {
                    corner.x *= fx;
                    corner.y *= fy;
                }
            }
            if let Some(rescued) = crate::rescue::attempt(luma, &sampling) {
                found += 1;
                let key = ContentKey {
                    raw: rescued.raw,
                    eci: EciEvidence::from_marks(rescued.eci),
                    fnc1: rescued.fnc1,
                };
                let rank = Rank {
                    attempt,
                    source: source::RESCUE,
                    index: 0,
                };
                let mut read = Read::new(Symbology::QrCode, key, Some(EngineKind::Rescue), rank)
                    .at(Some(candidate.corners));
                read.segments = Segments::Observed(rescued.segments);
                read.parsed = true;
                read.geometry = candidate_geometry(&candidate);
                self.store.collect(read);
                break; // first rescue wins — candidates are dedupes of one symbol
            }
        }
        stages.push(StageTrace {
            stage: "rescue".to_owned(),
            transforms_tried: tried,
            ms: started.elapsed().as_secs_f64() * 1_000.0,
            detections_found: found,
        });
        Ok(())
    }

    /// Keep a bounded set of rescue inputs (S5 cost is per-candidate);
    /// dedupe by symbol identity (version/ec/mask, photometry, orientation)
    /// — the same physical symbol surfaces across many attempts.
    fn collect_rescue(
        &mut self,
        candidates: Vec<RescueCandidate>,
        attempt_dims: (u32, u32),
        inverts: bool,
    ) {
        for mut candidate in candidates {
            if self.rescue_candidates.len() >= MAX_RESCUE_CANDIDATES {
                return;
            }
            if self.rescue_candidates.iter().any(|c| {
                (c.version, c.ec, c.mask, c.inverted, c.transposed)
                    == (
                        candidate.version,
                        candidate.ec,
                        candidate.mask,
                        inverts,
                        candidate.transposed,
                    )
            }) {
                continue;
            }
            if attempt_dims != self.orig {
                candidate.corners = rescaled(candidate.corners, attempt_dims, self.orig);
            }
            candidate.inverted = inverts;
            self.rescue_candidates.push(candidate);
        }
    }

    /// One attempt's engine pass, collected: its candidates re-derived while
    /// an unparsed QR Code read is in play, the refusals they prove
    /// recorded, the re-reads (which record their own), then the reads.
    /// Returns the reads the adapters delivered — what the trace counts.
    fn collect_attempt(
        &mut self,
        img: &LumaImage,
        outcome: engine::EngineOutcome,
        inverts: bool,
    ) -> u32 {
        self.panics = self.panics.saturating_add(outcome.panics);
        let delivered = u32::try_from(outcome.detections.len()).unwrap_or(u32::MAX);
        let dims = (img.width(), img.height());
        let index = self.next_attempt();
        let mut reads: Vec<(Read, Option<LocateHint>)> = outcome
            .detections
            .into_iter()
            .enumerate()
            .map(|(i, hit)| {
                let rank = Rank {
                    attempt: index,
                    source: match hit.engine {
                        EngineKind::Rxing => source::RXING,
                        EngineKind::Rqrr => source::RQRR,
                        EngineKind::Rescue => source::RESCUE,
                    },
                    index: u32::try_from(i).unwrap_or(u32::MAX),
                };
                self.read_of(hit, rank, dims, inverts)
            })
            .collect();
        let mut refusals: Vec<Refusal> = outcome
            .refused
            .into_iter()
            .map(|refusal| Refusal {
                place: refusal.place.map(|place| rescaled(place, dims, self.orig)),
                ..refusal
            })
            .collect();
        // the attempt's grids in original space, photometry stamped,
        // re-derived while an unparsed QR read is in play
        if reads.iter().any(|(read, _)| unparsed_qr(read)) || self.store.unparsed_qr() {
            let candidates: Vec<RescueCandidate> = outcome
                .rescue
                .iter()
                .map(|candidate| RescueCandidate {
                    corners: rescaled(candidate.corners, dims, self.orig),
                    inverted: inverts,
                    ..candidate.clone()
                })
                .collect();
            let (parses, refused) = Self::rederive(&candidates, index);
            reads.extend(parses.into_iter().map(|read| (read, None)));
            refusals.extend(refused);
        }
        // recorded before the re-reads: a read they withhold is not re-read
        for refusal in refusals {
            self.store.refuse(refusal, index);
        }
        let mut refusals = Vec::new();
        self.reread(img, &mut reads, &mut refusals);
        for refusal in refusals {
            self.store.refuse(refusal, index);
        }
        for (read, _) in reads {
            self.store.collect(read);
        }
        self.collect_rescue(outcome.rescue, dims, inverts);
        delivered
    }

    /// Run one stage as a fixed sequence of lazily-built attempts, each
    /// collected ([`Run::collect_attempt`]). Returns `Ok(true)` when the
    /// ladder should stop (budget mid-stage).
    fn stage(
        &mut self,
        name: &str,
        attempts: Vec<Attempt<'_>>,
        filter: engine::FormatFilter,
        stages: &mut Vec<StageTrace>,
    ) -> Result<bool> {
        let started = Instant::now();
        let mut tried = 0u32;
        let mut found_total = 0u32;
        let mut stop = false;
        for attempt in attempts {
            self.check_cancel()?;
            if self.out_of_budget() {
                stop = true;
                break;
            }
            let img = (attempt.build)();
            let outcome = engine::decode_filtered(&img, filter);
            // the trace counts the reads the adapters delivered
            found_total =
                found_total.saturating_add(self.collect_attempt(&img, outcome, attempt.inverts));
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

/// One lazily-built decode attempt: the transform closure plus whether its
/// chain photometrically INVERTS the symbol. The flag travels with adopted
/// geometry — structural/ISO sampling on the ORIGINAL luma must flip its
/// dark-test for grids that were measured on an inverted attempt (otherwise
/// a clean light-on-dark symbol reads finder integrity ≈ 0.33 and earns
/// bogus caps + hints — the review-found polarity lie).
struct Attempt<'a> {
    inverts: bool,
    build: Box<dyn Fn() -> LumaImage + 'a>,
}

impl<'a> Attempt<'a> {
    /// A polarity-preserving attempt (the common case).
    fn plain(build: impl Fn() -> LumaImage + 'a) -> Self {
        Self {
            inverts: false,
            build: Box::new(build),
        }
    }

    /// An attempt whose chain flips dark/light.
    fn inverting(build: impl Fn() -> LumaImage + 'a) -> Self {
        Self {
            inverts: true,
            build: Box::new(build),
        }
    }
}

/// S4 attempt list: the deep rungs (boosts + morphological closes), then
/// the size × contrast × binarization grid in fixed declared order (grid
/// combos duplicating S3 are skipped).
fn deep_attempts(luma: &LumaImage, longest: u32) -> Vec<Attempt<'_>> {
    type Op = fn(&LumaImage) -> LumaImage;
    let mut attempts: Vec<Attempt<'_>> = Vec::new();
    for rung in DEEP_RUNGS {
        // no deep rung flips polarity (boosts/morph-closes preserve it)
        attempts.push(Attempt::plain(move || rung.apply(luma)));
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
            for (op, inverts) in [
                (transform::otsu_threshold as Op, false),
                (transform::invert as Op, true),
            ] {
                let build = move || {
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
                };
                attempts.push(Attempt {
                    inverts,
                    build: Box::new(build),
                });
            }
        }
    }
    attempts
}

/// Four corners from one image's coordinate space to another's.
#[expect(
    clippy::cast_precision_loss,
    reason = "dimensions bounded by Limits::max_dimension — exact in f32"
)]
fn rescaled(mut corners: [Point; 4], from: (u32, u32), to: (u32, u32)) -> [Point; 4] {
    if from != to {
        let fx = to.0 as f32 / from.0 as f32;
        let fy = to.1 as f32 / from.1 as f32;
        for p in &mut corners {
            p.x *= fx;
            p.y *= fy;
        }
    }
    corners
}

/// Execute the ladder over normalized planes. `deadline` is the WHOLE-scan
/// wall-clock bound, shared with the scoring stage (computed by the caller).
pub(crate) fn run(
    planes: &SourcePlanes,
    config: &ScanConfig,
    cancel: &CancelToken,
    deadline: Option<Instant>,
) -> Result<LadderOutcome> {
    run_seeded(planes, config, cancel, deadline, Seed::default())
}

/// [`run`], starting from another walk's refusals, the contents its parses
/// read and the re-reads it spent — the opposite-background walk of an
/// alpha fallback.
pub(crate) fn run_seeded(
    planes: &SourcePlanes,
    config: &ScanConfig,
    cancel: &CancelToken,
    deadline: Option<Instant>,
    seed: Seed,
) -> Result<LadderOutcome> {
    let started = Instant::now();
    // Engines never see anything bigger than max_engine_side: one rxing
    // TryHarder pass on a huge image is an unbounded, uninterruptible burn.
    let work = transform::downscale_to(&planes.luma, config.max_engine_side);
    let mut run = Run::new(
        cancel,
        deadline,
        (planes.luma.width(), planes.luma.height()),
        seed,
    );
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
                vec![Attempt::plain(move || transform::downscale_to(luma, side))],
                engine::FormatFilter::All,
                &mut stages,
            )?;
            if stop || run.store.settled() {
                break 'ladder;
            }
        }

        // S2 — direct full resolution.
        if config.direct && !run.out_of_budget() {
            let stop = run.stage(
                "direct",
                vec![Attempt::plain(|| luma.clone())],
                engine::FormatFilter::All,
                &mut stages,
            )?;
            if stop || run.store.settled() {
                break 'ladder;
            }
        }

        // S3 — enhance: fixed transform set at full resolution.
        if config.enhance && !run.out_of_budget() {
            let mut attempts: Vec<Attempt<'_>> = vec![
                Attempt::plain(|| transform::otsu_threshold(luma)),
                Attempt::inverting(|| transform::invert(luma)),
                Attempt::plain(|| transform::contrast_stretch(luma)),
            ];
            if planes.has_color() {
                let cap = config.max_engine_side;
                for channel in [Channel::R, Channel::G, Channel::B] {
                    // LAZY: channel extraction walks the full-res RGB buffer
                    // — built only when this attempt actually runs, so the
                    // cost lands AFTER the per-attempt budget check
                    attempts.push(Attempt::plain(move || {
                        let plane = planes.channel(channel).unwrap_or_else(|| luma.clone());
                        transform::downscale_to(&plane, cap)
                    }));
                }
            }
            let stop = run.stage("enhance", attempts, engine::FormatFilter::All, &mut stages)?;
            if stop || run.store.settled() {
                break 'ladder;
            }
        }

        // S4 — deep: the curated boost rungs (v0.2 empirical known-good)
        // first, then the size × contrast × binarization grid.
        if config.deep && !run.out_of_budget() {
            // deep is QR-calibrated recovery: the multi-format detectors on
            // 17 rungs would starve the budget for nothing
            let _ = run.stage(
                "deep",
                deep_attempts(luma, longest),
                engine::FormatFilter::QrFamily,
                &mut stages,
            )?;
        }

        // S5 — rescue: erasure-aware RS over grids the engines detected
        // but could not decode (the logo-occlusion class). Only when the
        // ladder published nothing — a decoded symbol never needs rescuing.
        run.rescue_stage(luma, &mut stages)?;
    }
    run.finish(stages, started)
}

impl Run<'_> {
    /// End of the walk, every early exit included: the candidates collected
    /// in other attempts than the reads they locate or refuse (at most
    /// `MAX_RESCUE_CANDIDATES` clean re-derivations, no sampling — past the
    /// deadline too, after a last cancellation check), then the
    /// reconciliation of the whole read set and each content interpreted
    /// ONCE.
    fn finish(mut self, stages: Vec<StageTrace>, started: Instant) -> Result<LadderOutcome> {
        self.check_cancel()?;
        let attempt = self.next_attempt();
        let (parses, refusals) = Self::rederive(&self.rescue_candidates, attempt);
        for refusal in refusals {
            self.store.refuse(refusal, attempt);
        }
        for read in parses {
            self.store.collect(read);
        }
        // PRIMARY-detection contract: QR family first, then by earliest
        // read — on a flyer carrying a QR + a retail barcode, the scored
        // "primary" (detections[0]) never depends on detector order
        let reconciled = self.store.reconcile();
        let contents: Vec<(ContentKey, Reading)> = reconciled
            .iter()
            .map(|detection| {
                let reading = content::read(
                    &detection.key.raw,
                    &detection.key.eci,
                    &detection.segments,
                    detection.engine_reading,
                    detection.symbology.is_qr_family(),
                );
                (detection.key.clone(), reading)
            })
            .collect();
        let merged = reconciled
            .iter()
            .zip(&contents)
            .map(|(detection, (_, reading))| merged_detection(detection, reading))
            .collect();
        Ok(LadderOutcome {
            merged,
            contents,
            store: self.store,
            rereads: self.rereads,
            trace: PipelineTrace {
                stages,
                engine_panics: self.panics,
                total_ms: started.elapsed().as_secs_f64() * 1_000.0,
            },
        })
    }
}

/// A reconciled content as the score and the report see it.
fn merged_detection(detection: &Reconciled, reading: &Reading) -> MergedDetection {
    let (masked_stream, version, ec, mask) = match &detection.geometry.stream {
        Some((stream, version, ec, mask)) => {
            (Some(stream.clone()), Some(*version), Some(*ec), Some(*mask))
        }
        None => (None, None, detection.geometry.ec, None),
    };
    MergedDetection {
        symbology: detection.symbology,
        raw: detection.key.raw.clone(),
        text: reading.text.clone(),
        charset: reading.charset,
        masked_stream,
        corners: detection.geometry.corners.map(|(corners, _)| corners),
        version,
        ec,
        mask,
        fnc1: detection.key.fnc1 == Fnc1::First,
        photometric_inverted: detection
            .geometry
            .corners
            .is_some_and(|(_, inverted)| inverted),
        engines: detection.engines.clone(),
    }
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

    /// Arbitrary-angle robustness is a measured engine CAPABILITY — pin it.
    /// The 2026-07-08 H-12 probes could not construct a failing synthetic:
    /// clean and degraded renders decode at every angle down to 2 px
    /// modules (the real-photo qrcode-5 losses at 15–45° need photo noise
    /// our synthesis cannot reproduce, and a derotate rung was REFUTED as
    /// untestable). This test freezes that capability against engine bumps:
    /// if an rqrr/rxing upgrade breaks diagonal detection, this is the
    /// tripwire — not a corpus re-run six weeks later.
    #[test]
    fn rotated_synthetic_decodes_at_arbitrary_angles() {
        let text = "arbitrary-angle capability pin";
        let code = qrcode::QrCode::with_version(
            text.as_bytes(),
            qrcode::Version::Normal(5),
            qrcode::EcLevel::M,
        )
        .unwrap();
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(3, 3)
            .build();
        let base = LumaImage::new(img.to_vec(), img.width(), img.height());
        for angle in [15.0_f32, 30.0, 45.0] {
            let rotated = crate::score::warp::rotate(&base, angle);
            let planes = normalize(
                &ImageInput::luma8(rotated.data(), rotated.width(), rotated.height()),
                &Limits::default(),
            )
            .unwrap();
            // budget-free: capability, not latency, is what this pins
            let mut config = ScanConfig::full();
            config.budget_ms = None;
            let outcome = run(&planes, &config, &CancelToken::new(), None).unwrap();
            assert!(
                outcome.merged.iter().any(|m| m.text == text),
                "{angle}°: engines lost arbitrary-angle detection — an engine \
                 regression, not an image problem (rotation is synthetic-clean)"
            );
        }
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
    fn probe_rescue_viability_occluded_disk() {
        // does rqrr still hand us grid + raw stream where decode fails?
        for pct in [20u32, 22, 26, 30] {
            let path = format!("/tmp/rescue-h-{pct}.png");
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
            let luma = &planes.luma;
            let width = luma.width() as usize;
            let data = luma.data();
            let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(
                width,
                luma.height() as usize,
                |x, y| data[y * width + x],
            );
            for grid in prepared.detect_grids() {
                let mut raw = Vec::new();
                let decode_ok = grid.decode_to(&mut raw).is_ok();
                let raw_data = grid.get_raw_data();
                println!(
                    "disk {pct}%: decode_to={} get_raw_data={} meta={:?}",
                    decode_ok,
                    raw_data.is_ok(),
                    raw_data
                        .as_ref()
                        .ok()
                        .map(|(m, _)| (m.version.0, m.ecc_level, m.mask)),
                );
            }
        }
        println!("probe done");
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

    /// Kanji-MODE symbols: both engines report the original Shift-JIS
    /// bytes (rxing's re-encoded text is never the payload). Same symbol
    /// MUST merge into one detection, raw = the original SJIS bytes.
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
        assert_eq!(d.raw, sjis, "raw must be the ORIGINAL bytes");
        assert_eq!(d.text, "こんにちは");
        assert_eq!(d.charset, crate::report::Charset::ShiftJis);
        assert_eq!(d.engines, vec![EngineKind::Rxing, EngineKind::Rqrr]);
    }
}

#[cfg(test)]
mod content_readings {
    //! The ladder's account of each detection's content, on symbols encoded
    //! from explicit segment streams: one reading per detection, its label
    //! on the wire.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::input::{ImageInput, Limits};
    use crate::report::Charset;
    use crate::transform::normalize;

    /// Walk the camera-frame stages over a v3-M symbol built from `push`.
    fn walk(
        push: impl FnOnce(&mut qrcode::bits::Bits) -> qrcode::types::QrResult<()>,
    ) -> LadderOutcome {
        let mut bits = qrcode::bits::Bits::new(qrcode::Version::Normal(3));
        push(&mut bits).unwrap();
        bits.push_terminator(qrcode::EcLevel::M).unwrap();
        let code = qrcode::QrCode::with_bits(bits, qrcode::EcLevel::M).unwrap();
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(6, 6)
            .build();
        let planes = normalize(
            &ImageInput::luma8(img.as_raw(), img.width(), img.height()),
            &Limits::default(),
        )
        .unwrap();
        let mut config = ScanConfig::frame();
        config.budget_ms = None;
        run(&planes, &config, &CancelToken::new(), None).unwrap()
    }

    /// The one detection of a single-symbol walk, with its content key.
    fn single(outcome: &LadderOutcome) -> (&MergedDetection, &ContentKey) {
        assert_eq!(outcome.merged.len(), 1, "{:?}", outcome.merged);
        assert_eq!(outcome.contents.len(), 1);
        let (key, reading) = &outcome.contents[0];
        let d = &outcome.merged[0];
        // the published triple is the one reading of the reconciled content
        assert_eq!(
            (&key.raw, &reading.text, reading.charset),
            (&d.raw, &d.text, d.charset)
        );
        (d, key)
    }

    fn located(marks: &[(u32, usize)]) -> EciEvidence {
        EciEvidence::Located(
            marks
                .iter()
                .map(|&(value, start)| content::EciMark { value, start })
                .collect(),
        )
    }

    #[test]
    fn declared_eci_text_is_labelled_eci_not_legacy() {
        let outcome = walk(|b| {
            b.push_eci_designator(4)?;
            b.push_byte_data(&[0xA1])
        });
        let (d, key) = single(&outcome);
        assert_eq!(
            (d.raw.as_slice(), d.text.as_str()),
            (&[0xA1][..], "\u{104}")
        );
        assert_eq!(
            key.eci,
            located(&[(4, 0)]),
            "rqrr's re-derivation located it"
        );
        assert_eq!(d.charset, Charset::Eci, "no decoder label reads ISO 8859-2");
    }

    #[test]
    fn eci3_c1_bytes_are_eci_without_a_windows_1252_claim() {
        let outcome = walk(|b| {
            b.push_eci_designator(3)?;
            b.push_byte_data(&[0x35, 0x80])
        });
        let (d, _) = single(&outcome);
        assert_eq!(d.text, "5\u{80}");
        assert_eq!(d.charset, Charset::Eci, "latin1 would decode 0x80 as '€'");
    }

    #[test]
    fn declarations_without_a_reading_are_marked_unknown() {
        let cases: [(&str, u32, &[u8], &str); 3] = [
            ("unsupported", 999, &[0xA1], "\u{FFFD}"),
            ("undecodable", 26, &[0xA1], "\u{FFFD}"),
            (
                "binary",
                899,
                &[0x00, 0xFF, 0x80, 0x7F],
                "\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}",
            ),
        ];
        for (name, value, bytes, text) in cases {
            let outcome = walk(|b| {
                b.push_eci_designator(value)?;
                b.push_byte_data(bytes)
            });
            let (d, _) = single(&outcome);
            assert_eq!(d.raw, bytes, "{name}");
            assert_eq!(d.text, text, "{name}: marked, never transcribed");
            assert_eq!(d.charset, Charset::Unknown, "{name}");
        }
    }

    #[test]
    fn kanji_after_eci26_is_marked_not_shift_jis() {
        let outcome = walk(|b| {
            b.push_eci_designator(26)?;
            b.push_byte_data("é".as_bytes())?;
            b.push_kanji_data(&[0x82, 0xB1, 0x82, 0xF1])
        });
        let (d, _) = single(&outcome);
        assert_eq!(d.raw, vec![0xC3, 0xA9, 0x82, 0xB1, 0x82, 0xF1]);
        // WHATWG UTF-8: one U+FFFD per maximal ill-formed subpart
        assert_eq!(d.text, String::from_utf8_lossy(&d.raw));
        assert!(d.text.starts_with('é') && d.text.contains('\u{FFFD}'));
        assert_eq!(d.charset, Charset::Unknown);
    }

    #[test]
    fn kanji_before_the_first_designator_is_iso_8859_1() {
        // the Kanji pair travels as Shift_JIS bytes, but a symbol with a
        // designator reads every byte before it as ISO/IEC 8859-1
        let outcome = walk(|b| {
            b.push_kanji_data(&[0x82, 0xB1])?;
            b.push_eci_designator(26)?;
            b.push_byte_data(b"a")
        });
        let (d, key) = single(&outcome);
        assert_eq!(d.raw, vec![0x82, 0xB1, b'a']);
        assert_eq!(key.eci, located(&[(26, 2)]));
        assert_eq!(d.text, "\u{82}\u{B1}a");
        assert_eq!(d.charset, Charset::Eci, "utf8 cannot reproduce it");
    }

    #[test]
    fn unmarked_payloads_keep_the_legacy_resolver() {
        let outcome = walk(|b| b.push_byte_data(&[0x35, 0x80]));
        let (d, key) = single(&outcome);
        assert_eq!(d.text, "5€");
        assert_eq!(key.eci, EciEvidence::NoEci, "both engines observed none");
        assert_eq!(d.charset, Charset::Latin1);
        assert_eq!(d.engines, vec![EngineKind::Rxing, EngineKind::Rqrr]);
    }

    #[test]
    fn kanji_segments_choose_shift_jis() {
        // the parse observes the Kanji segment: Shift_JIS over all of raw
        let outcome = walk(|b| {
            b.push_byte_data(b"QR:")?;
            b.push_kanji_data(&[0x93, 0xFA, 0x96, 0x7B])
        });
        let (d, key) = single(&outcome);
        assert_eq!(key.eci, EciEvidence::NoEci);
        assert_eq!((d.text.as_str(), d.charset), ("QR:日本", Charset::ShiftJis));
    }
}

#[cfg(test)]
mod ladder_mutant_kills {
    //! Surgical traps for the weekly mutation survivors on the decode ladder
    //! (the product's critical path). Each test names the mutant(s) it kills
    //! by `line:col` where one exists. The candidate, re-read, rescue and
    //! book-keeping logic is exercised directly (private methods, reachable
    //! from this in-crate child module); the stage-gating booleans in `run`
    //! are driven through synthetic in-memory planes with `budget_ms: None`
    //! so the walk is deterministic — never wall-clock-dependent.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]

    use super::*;
    use crate::engine::content::EciMark;
    use crate::input::{ImageInput, Limits};
    use crate::report::EcLevel;
    use crate::transform::normalize;

    fn fresh_run(cancel: &CancelToken, orig: (u32, u32)) -> Run<'_> {
        Run::new(cancel, None, orig, Seed::default())
    }

    fn qr_png(content: &str, module: u32) -> Vec<u8> {
        let code =
            qrcode::QrCode::with_error_correction_level(content, qrcode::EcLevel::Q).unwrap();
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(module, module)
            .build();
        let mut buf = Vec::new();
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    fn stage_names(outcome: &LadderOutcome) -> Vec<&str> {
        outcome
            .trace
            .stages
            .iter()
            .map(|s| s.stage.as_str())
            .collect()
    }

    fn eci_mark(value: u32, start: usize) -> EciEvidence {
        EciEvidence::Located(vec![EciMark { value, start }])
    }

    /// The engine reads of one decode pass as reads of `run`'s attempt
    /// `attempt` (full-resolution, upright).
    fn reads_of(
        run: &Run<'_>,
        detections: Vec<RawDetection>,
        attempt: u32,
        dims: (u32, u32),
    ) -> Vec<(Read, Option<LocateHint>)> {
        detections
            .into_iter()
            .enumerate()
            .map(|(i, hit)| {
                let rank = Rank {
                    attempt,
                    source: 0,
                    index: u32::try_from(i).unwrap(),
                };
                run.read_of(hit, rank, dims, false)
            })
            .collect()
    }

    // ---- Run::read_of ----

    #[test]
    fn photometry_travels_with_the_corners_only() {
        // `&&` -> `||` on the photometry: an upright attempt with corners is
        // not inverted; an inverting one is; a read without corners carries
        // no photometry at all
        let cancel = CancelToken::new();
        let run = fresh_run(&cancel, (100, 100));
        let mut hit = RawDetection::qr(b"x", EciEvidence::NoEci, Fnc1::None, EngineKind::Rqrr);
        hit.corners = Some([Point { x: 2.0, y: 3.0 }; 4]);
        let rank = Rank::default();
        let (upright, _) = run.read_of(hit.clone(), rank, (100, 100), false);
        let (inverted, _) = run.read_of(hit.clone(), rank, (100, 100), true);
        assert_eq!(upright.geometry.corners.map(|(_, i)| i), Some(false));
        assert_eq!(inverted.geometry.corners.map(|(_, i)| i), Some(true));
        hit.corners = None;
        let (bare, _) = run.read_of(hit, rank, (100, 100), true);
        assert!(bare.geometry.corners.is_none());
    }

    #[test]
    fn a_read_is_placed_in_original_space() {
        // orig 200x100 over an attempt of 50x50: fx = 4, fy = 2
        let cancel = CancelToken::new();
        let run = fresh_run(&cancel, (200, 100));
        let mut hit = RawDetection::qr(b"x", EciEvidence::NoEci, Fnc1::None, EngineKind::Rxing);
        hit.location = Some([Point { x: 10.0, y: 10.0 }; 4]);
        hit.ec = Some(EcLevel::Q);
        let (read, _) = run.read_of(hit, Rank::default(), (50, 50), false);
        let corner = read.location.unwrap()[0];
        assert!((corner.x - 40.0).abs() < 1e-3 && (corner.y - 20.0).abs() < 1e-3);
        assert_eq!(read.places.len(), 1);
        assert_eq!(read.geometry.ec, Some(EcLevel::Q), "the port's EC level");
    }

    // ---- Run::rederive (candidates) ----

    /// The ECI 26 + FNC1-first GTIN symbol (rqrr's grid is a candidate).
    fn eci26_gs1() -> LumaImage {
        crate::rescue::test_symbols::from_segments(
            qrcode::Version::Normal(3),
            qrcode::EcLevel::M,
            |b| {
                b.push_eci_designator(26)?;
                b.push_fnc1_first_position()?;
                b.push_numeric_data(b"0109506000134352")
            },
        )
    }

    #[test]
    fn a_rederived_candidate_locates_a_signalled_read_and_nothing_else() {
        // ECI 26 + FNC1 first: rqrr cannot decode FNC1 (its grid is a
        // candidate), rxing's port signals the ECI
        let luma = eci26_gs1();
        let dims = (luma.width(), luma.height());
        let outcome = engine::decode_filtered(&luma, engine::FormatFilter::All);
        assert_eq!(outcome.rescue.len(), 1, "rqrr's grid");
        let (parses, refusals) = Run::rederive(&outcome.rescue, 0);
        assert!(refusals.is_empty());
        let [parse] = &parses[..] else {
            panic!("one candidate parse: {parses:?}");
        };
        assert_eq!(parse.key.eci, eci_mark(26, 0));
        assert_eq!(parse.key.fnc1, Fnc1::First);
        assert!(
            parse.parsed && parse.engines.is_empty(),
            "a parse, no engine"
        );
        // alone it publishes nothing; with the port's read it locates it
        let cancel = CancelToken::new();
        let mut run = fresh_run(&cancel, dims);
        run.store.collect(parse.clone());
        assert!(run.store.reconcile().is_empty());
        for (read, _) in reads_of(&run, outcome.detections, 0, dims) {
            run.store.collect(read);
        }
        let found = run.store.reconcile();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key.eci, eci_mark(26, 0), "located over signalled");
        assert_eq!(found[0].engines, vec![EngineKind::Rxing]);
        assert!(found[0].geometry.corners.is_some() && found[0].geometry.stream.is_some());
    }

    #[test]
    fn at_most_four_candidates_are_re_derived_per_pass() {
        // five candidates, only the fifth would parse: nothing is parsed
        let luma = eci26_gs1();
        let outcome = engine::decode_filtered(&luma, engine::FormatFilter::All);
        let dud = crate::rescue::RescueCandidate {
            stream: engine::MaskedStream {
                bits: Vec::new(),
                bit_len: 0,
            },
            corners: [Point { x: 0.0, y: 0.0 }; 4],
            version: 0,
            ec: EcLevel::M,
            mask: 0,
            inverted: false,
            transposed: false,
            rederived: None,
        };
        let mut candidates = vec![dud; MAX_RESCUE_CANDIDATES];
        candidates.push(outcome.rescue[0].clone());
        let (parses, refusals) = Run::rederive(&candidates, 0);
        assert!(parses.is_empty() && refusals.is_empty());
        // fourth, it does
        candidates.remove(0);
        assert_eq!(Run::rederive(&candidates, 0).0.len(), 1);
    }

    #[test]
    fn the_end_of_walk_pass_locates_and_refuses_across_attempts() {
        // rqrr's grid collected in one attempt, rxing's read in another:
        // only the end-of-walk pass brings them together
        let cancel = CancelToken::new();
        let luma = eci26_gs1();
        let dims = (luma.width(), luma.height());
        let outcome = engine::decode_filtered(&luma, engine::FormatFilter::All);
        let mut run = fresh_run(&cancel, dims);
        run.collect_rescue(outcome.rescue, dims, false);
        for (read, _) in reads_of(&run, outcome.detections, 1, dims) {
            run.store.collect(read);
        }
        let walked = run.finish(Vec::new(), Instant::now()).unwrap();
        assert_eq!(walked.merged.len(), 1);
        let d = &walked.merged[0];
        assert_eq!(d.text, "0109506000134352");
        assert_eq!(d.engines, vec![EngineKind::Rxing]);
        assert!(d.fnc1 && d.corners.is_some());
        assert_eq!(walked.contents[0].0.eci, eci_mark(26, 0));
        // FNC1 second then first: the collected grid proves the stream
        // malformed and withholds the port's GS1 read where the grid is
        let data = crate::rescue::test_symbols::stream(
            "1001 00100101 0101 0100 00000011 01100001 01100010 01100011 0000",
        );
        let luma = crate::rescue::test_symbols::from_codewords(
            qrcode::Version::Normal(1),
            qrcode::EcLevel::M,
            &data,
        );
        let dims = (luma.width(), luma.height());
        let outcome = engine::decode_filtered(&luma, engine::FormatFilter::All);
        let mut run = fresh_run(&cancel, dims);
        run.collect_rescue(outcome.rescue, dims, false);
        for (read, _) in reads_of(&run, outcome.detections, 1, dims) {
            run.store.collect(read);
        }
        assert_eq!(run.store.reconcile().len(), 1, "the port's GS1 read");
        let walked = run.finish(Vec::new(), Instant::now()).unwrap();
        assert!(walked.merged.is_empty(), "{:?}", walked.merged);
        // and a cancel raised by then is honoured first
        let cancelled = CancelToken::new();
        cancelled.cancel();
        let err = fresh_run(&cancelled, dims)
            .finish(Vec::new(), Instant::now())
            .unwrap_err();
        assert_eq!(err.code(), "QRS-005");
    }

    #[test]
    fn a_cancel_is_honoured_before_the_end_of_walk_pass() {
        // the end-of-walk re-derivation runs after the last attempt's
        // cancel check: a cancel raised by then is honoured first (every
        // stage off here, so no attempt checks it at all)
        let data = vec![255u8; 64 * 64];
        let planes = normalize(&ImageInput::luma8(&data, 64, 64), &Limits::default()).unwrap();
        let mut config = ScanConfig::full();
        (config.pyramid, config.direct, config.enhance, config.deep) = (false, false, false, false);
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = run(&planes, &config, &cancel, None).unwrap_err();
        assert_eq!(err.code(), "QRS-005");
    }

    // ---- Run::reread ----

    #[test]
    fn the_re_read_locates_a_port_read_no_parse_of_the_attempt_carries() {
        // ECI 4 [A1]: rxing's port read signals a designator; the re-read at
        // its corners locates it — unless rqrr's parsed read of the same
        // attempt carries the content already
        let luma = crate::rescue::test_symbols::from_segments(
            qrcode::Version::Normal(3),
            qrcode::EcLevel::M,
            |b| {
                b.push_eci_designator(4)?;
                b.push_byte_data(&[0xA1])
            },
        );
        let dims = (luma.width(), luma.height());
        let outcome = engine::decode_filtered(&luma, engine::FormatFilter::All);
        let cancel = CancelToken::new();
        let port: Vec<RawDetection> = outcome
            .detections
            .iter()
            .filter(|d| d.engine == EngineKind::Rxing)
            .cloned()
            .collect();
        let mut run = fresh_run(&cancel, dims);
        let mut reads = reads_of(&run, port.clone(), 0, dims);
        let mut refusals = Vec::new();
        run.reread(&luma, &mut reads, &mut refusals);
        assert_eq!(run.rereads, 1);
        assert!(refusals.is_empty());
        let (read, _) = &reads[0];
        assert_eq!(read.key.eci, eci_mark(4, 0));
        assert!(read.parsed && read.engines == vec![(EngineKind::Rxing, read.rank)]);
        assert!(
            read.geometry.corners.is_none(),
            "its geometry stays internal"
        );
        // carried by rqrr's parse of the same attempt: no re-read spent
        let mut run = fresh_run(&cancel, dims);
        let mut reads = reads_of(&run, outcome.detections, 0, dims);
        run.reread(&luma, &mut reads, &mut refusals);
        assert_eq!(run.rereads, 0);
        // the scan's re-reads are bounded
        let mut run = fresh_run(&cancel, dims);
        run.rereads = MAX_REREADS;
        let mut reads = reads_of(&run, port, 0, dims);
        run.reread(&luma, &mut reads, &mut refusals);
        assert_eq!(reads[0].0.key.eci, EciEvidence::Signalled, "none left");
        assert_eq!(run.rereads, MAX_REREADS);
    }

    /// v1-M: ECI 1000000 (above the register) then byte "abc" — rqrr
    /// decodes "abc", rxing's port signals a designator and reads "abc",
    /// the scanner's parser refuses the stream.
    fn eci_above_the_register_abc() -> LumaImage {
        let data = crate::rescue::test_symbols::stream(
            "0111 11001111 01000010 01000000 0100 00000011 01100001 01100010 01100011 0000",
        );
        crate::rescue::test_symbols::from_codewords(
            qrcode::Version::Normal(1),
            qrcode::EcLevel::M,
            &data,
        )
    }

    #[test]
    fn a_read_a_recorded_refusal_withholds_is_not_re_read() {
        // the port read of a refused symbol is withheld anyway — re-reading
        // it in every attempt would spend the scan's re-reads on one
        // malformed symbol. rqrr's refused decode is recorded (the port's
        // reading and rqrr's bytes, at rqrr's bounds); a later attempt's
        // port read lies inside: no re-read is spent on it
        let luma = eci_above_the_register_abc();
        let dims = (luma.width(), luma.height());
        let outcome = engine::decode_filtered(&luma, engine::FormatFilter::All);
        let [refusal] = &outcome.refused[..] else {
            panic!("rqrr's refused decode: {:?}", outcome.refused);
        };
        let port: Vec<RawDetection> = outcome
            .detections
            .into_iter()
            .filter(|d| d.engine == EngineKind::Rxing)
            .collect();
        assert!(
            !port.is_empty() && port.iter().all(|d| d.locate.is_some()),
            "the port's read is a re-read candidate: {port:?}"
        );
        let cancel = CancelToken::new();
        let mut run = fresh_run(&cancel, dims);
        run.store.refuse(refusal.clone(), 0);
        let mut reads = reads_of(&run, port.clone(), 1, dims);
        let mut refusals = Vec::new();
        run.reread(&luma, &mut reads, &mut refusals);
        assert_eq!(run.rereads, 0, "withheld anyway");
        assert!(refusals.is_empty() && reads.len() == port.len());
        // without that record, the re-read runs — and refuses it
        let mut run = fresh_run(&cancel, dims);
        let mut reads = reads_of(&run, port, 1, dims);
        run.reread(&luma, &mut reads, &mut refusals);
        assert_eq!(run.rereads, 1);
        assert_eq!(refusals.len(), 1, "{refusals:?}");
    }

    #[test]
    fn a_symbol_refused_in_every_attempt_spends_no_re_read() {
        // the whole walk over that symbol: rqrr refuses it wherever it
        // decodes it and the port reads it in every attempt — the refusal is
        // recorded before the attempt's re-reads, so none is spent on it
        let luma = eci_above_the_register_abc();
        let planes = normalize(
            &ImageInput::luma8(luma.data(), luma.width(), luma.height()),
            &Limits::default(),
        )
        .unwrap();
        let mut config = ScanConfig::full();
        config.budget_ms = None;
        let outcome = run(&planes, &config, &CancelToken::new(), None).unwrap();
        assert!(outcome.merged.is_empty(), "{:?}", outcome.merged);
        assert_eq!(outcome.rereads, 0, "{:?}", outcome.trace.stages);
        // refused at every attempt scale, one record
        assert_eq!(outcome.store.records(), 1, "{:?}", outcome.trace.stages);
    }

    /// A 100 px square at `x`.
    fn square_at(x: f32) -> [Point; 4] {
        [
            Point { x, y: 0.0 },
            Point {
                x: x + 100.0,
                y: 0.0,
            },
            Point {
                x: x + 100.0,
                y: 100.0,
            },
            Point { x, y: 100.0 },
        ]
    }

    #[test]
    fn only_a_parse_where_the_read_lies_carries_it() {
        // a parsed read of the attempt carries a port read only where it
        // contains it. ECI 4 [A1]: a parse of the same content made
        // elsewhere (another symbol) leaves the port read to its own
        // re-read, which locates it; a parse where the read lies carries it
        let luma = crate::rescue::test_symbols::from_segments(
            qrcode::Version::Normal(3),
            qrcode::EcLevel::M,
            |b| {
                b.push_eci_designator(4)?;
                b.push_byte_data(&[0xA1])
            },
        );
        let dims = (luma.width(), luma.height());
        let port: Vec<RawDetection> = engine::decode_filtered(&luma, engine::FormatFilter::All)
            .detections
            .into_iter()
            .filter(|d| d.engine == EngineKind::Rxing)
            .collect();
        let parse_at = |place: Option<[Point; 4]>| {
            let key = ContentKey {
                raw: vec![0xA1],
                eci: eci_mark(4, 0),
                fnc1: Fnc1::None,
            };
            let rank = Rank {
                attempt: 0,
                source: source::RQRR,
                index: 0,
            };
            let mut parse =
                Read::new(Symbology::QrCode, key, Some(EngineKind::Rqrr), rank).at(place);
            parse.parsed = true;
            (parse, None)
        };
        let cancel = CancelToken::new();
        let mut refusals = Vec::new();
        let mut run = fresh_run(&cancel, dims);
        let mut reads = reads_of(&run, port.clone(), 0, dims);
        reads.push(parse_at(Some(square_at(5_000.0))));
        run.reread(&luma, &mut reads, &mut refusals);
        assert_eq!(run.rereads, 1, "a parse elsewhere carries nothing");
        assert_eq!(reads[0].0.key.eci, eci_mark(4, 0), "located by its re-read");
        let mut run = fresh_run(&cancel, dims);
        let mut reads = reads_of(&run, port, 0, dims);
        let here = reads[0].0.location;
        reads.push(parse_at(here));
        run.reread(&luma, &mut reads, &mut refusals);
        assert_eq!(run.rereads, 0, "the parse where it lies carries it");
        assert!(refusals.is_empty());
    }

    // ---- Run::collect_rescue ----

    #[test]
    fn collect_rescue_dedupes_by_symbol_identity() {
        // `==` -> `!=` (dedup identity). Two candidates with identical
        // (version, ec, mask, inverts) collapse to one.
        let cancel = CancelToken::new();
        let mut run = fresh_run(&cancel, (100, 100));
        let twin = || crate::rescue::RescueCandidate {
            stream: engine::MaskedStream {
                bits: vec![0u8; 8],
                bit_len: 64,
            },
            corners: [Point { x: 5.0, y: 5.0 }; 4],
            version: 3,
            ec: EcLevel::Q,
            mask: 2,
            inverted: false,
            transposed: false,
            rederived: None,
        };
        run.collect_rescue(vec![twin(), twin()], (100, 100), false);
        assert_eq!(run.rescue_candidates.len(), 1, "`!=` keeps the duplicate");
    }

    #[test]
    fn collect_rescue_rescales_corners_into_original_space() {
        // orig 200x100 over an attempt of 50x50 gives fx=4, fy=2
        // (deliberately distinct); a (10,10) corner must land at (40,20).
        let cancel = CancelToken::new();
        let mut run = fresh_run(&cancel, (200, 100));
        let candidate = crate::rescue::RescueCandidate {
            stream: engine::MaskedStream {
                bits: vec![0u8; 8],
                bit_len: 64,
            },
            corners: [Point { x: 10.0, y: 10.0 }; 4],
            version: 1,
            ec: EcLevel::M,
            mask: 0,
            inverted: false,
            transposed: false,
            rederived: None,
        };
        run.collect_rescue(vec![candidate], (50, 50), false);
        assert_eq!(run.rescue_candidates.len(), 1);
        let corner = run.rescue_candidates[0].corners[0];
        assert!((corner.x - 40.0).abs() < 1e-3, "{}", corner.x);
        assert!((corner.y - 20.0).abs() < 1e-3, "{}", corner.y);
    }

    #[test]
    fn collect_rescue_leaves_full_res_corners_untouched() {
        let cancel = CancelToken::new();
        let mut run = fresh_run(&cancel, (120, 90));
        let candidate = crate::rescue::RescueCandidate {
            stream: engine::MaskedStream {
                bits: vec![0u8; 8],
                bit_len: 64,
            },
            corners: [Point { x: 12.0, y: 7.0 }; 4],
            version: 1,
            ec: EcLevel::M,
            mask: 0,
            inverted: false,
            transposed: false,
            rederived: None,
        };
        run.collect_rescue(vec![candidate], (120, 90), false);
        let corner = run.rescue_candidates[0].corners[0];
        assert!((corner.x - 12.0).abs() < 1e-3 && (corner.y - 7.0).abs() < 1e-3);
    }

    // ---- Run::rescue_stage ----

    #[test]
    fn rescue_stage_counts_and_times_every_candidate() {
        // `tried += 1` -> `*=` (freezes at 0) + `ms * 1000` -> `+ 1000`.
        // Three version-0 candidates fail `rescue::attempt` at its 1..=40
        // range check (no parsing, no panic): each TRIED, none FOUND.
        let cancel = CancelToken::new();
        let luma = LumaImage::new(vec![255u8; 100 * 100], 100, 100);
        let dud = || crate::rescue::RescueCandidate {
            stream: engine::MaskedStream {
                bits: Vec::new(),
                bit_len: 0,
            },
            corners: [Point { x: 0.0, y: 0.0 }; 4],
            version: 0,
            ec: EcLevel::M,
            mask: 0,
            inverted: false,
            transposed: false,
            rederived: None,
        };
        let mut run = fresh_run(&cancel, (100, 100));
        run.rescue_candidates = vec![dud(), dud(), dud()];
        let mut stages = Vec::new();
        run.rescue_stage(&luma, &mut stages).unwrap();
        let rescue = stages
            .iter()
            .find(|s| s.stage == "rescue")
            .expect("rescue stage pushed");
        assert_eq!(rescue.transforms_tried, 3, "`*=` freezes tried at 0");
        assert_eq!(rescue.detections_found, 0);
        assert!(rescue.ms < 100.0, "`+ 1000` reads ~1000ms ({})", rescue.ms);
        // a walk that published something never runs it
        let mut run = fresh_run(&cancel, (100, 100));
        run.rescue_candidates = vec![dud()];
        let rank = Rank::default();
        let key = ContentKey {
            raw: b"x".to_vec(),
            eci: EciEvidence::NoEci,
            fnc1: Fnc1::None,
        };
        run.store.collect(Read::new(
            Symbology::QrCode,
            key,
            Some(EngineKind::Rxing),
            rank,
        ));
        let mut stages = Vec::new();
        run.rescue_stage(&luma, &mut stages).unwrap();
        assert!(stages.is_empty());
    }

    #[test]
    fn rescue_stage_reports_a_real_recovery() {
        // `found += 1` -> `*=` (freezes at 0). The occluded fixture is
        // recovered ONLY by S5 rescue; with budget_ms None the walk is fully
        // deterministic (no wall-clock cut).
        let bytes = std::fs::read(format!(
            "{}/../../fixtures/degraded/logo-occluded-rescue.png",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
        let mut config = ScanConfig::full();
        config.budget_ms = None;
        let outcome = run(&planes, &config, &CancelToken::new(), None).unwrap();
        assert_eq!(outcome.merged.len(), 1, "S5 recovers the occluded symbol");
        assert_eq!(outcome.merged[0].engines, vec![EngineKind::Rescue]);
        let rescue = outcome
            .trace
            .stages
            .iter()
            .find(|s| s.stage == "rescue")
            .expect("rescue stage ran");
        assert_eq!(rescue.detections_found, 1, "`*=` freezes found at 0");
        assert!(
            rescue.transforms_tried >= 1,
            "the winning candidate was tried"
        );
    }

    // ---- Run::stage ----

    #[test]
    fn stage_accumulates_raw_detections_found() {
        // `found_total += ..` -> `*=` (freezes at 0). A clean QR decodes,
        // so the deciding stage reports at least one raw hit.
        let bytes = qr_png("https://qrcode-ai.com/found-count-pin", 6);
        let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
        let mut config = ScanConfig::full();
        config.budget_ms = None;
        let outcome = run(&planes, &config, &CancelToken::new(), None).unwrap();
        assert_eq!(outcome.merged.len(), 1);
        let found: u32 = outcome
            .trace
            .stages
            .iter()
            .map(|s| s.detections_found)
            .sum();
        assert!(
            found >= 1,
            "`*=` freezes every stage's detections_found at 0"
        );
    }

    #[test]
    fn the_trace_counts_every_hit_past_the_read_bound() {
        // one attempt delivering 65 distinct hits — the trace counts
        // all 65 (what `stage` adds to `detections_found`), 64 distinct
        // reads are reconciled (the 65th counted), at most 16 published
        let cancel = CancelToken::new();
        let img = LumaImage::new(vec![255u8; 64 * 64], 64, 64);
        let mut run = fresh_run(&cancel, (64, 64));
        let outcome = engine::EngineOutcome {
            detections: (0..65)
                .map(|i| {
                    RawDetection::qr(
                        format!("symbol-{i}").as_bytes(),
                        EciEvidence::NoEci,
                        Fnc1::None,
                        EngineKind::Rxing,
                    )
                })
                .collect(),
            ..engine::EngineOutcome::default()
        };
        assert_eq!(run.collect_attempt(&img, outcome, false), 65);
        assert_eq!(run.store.dropped, 1, "the 65th distinct read is counted");
        let walked = run.finish(Vec::new(), Instant::now()).unwrap();
        assert_eq!(
            walked.merged.len(),
            crate::engine::reconcile::MAX_DETECTIONS
        );
    }

    #[test]
    fn stage_ms_stays_within_total_ms() {
        // stage `ms * 1000` -> `+ 1000` and total `ms * 1000` -> `+ 1000` /
        // `/ 1000`. Structural invariant: every stage is a sub-interval of
        // the whole run.
        let data = vec![255u8; 96 * 96];
        let planes = normalize(&ImageInput::luma8(&data, 96, 96), &Limits::default()).unwrap();
        let mut config = ScanConfig::full();
        config.budget_ms = None;
        let outcome = run(&planes, &config, &CancelToken::new(), None).unwrap();
        let total = outcome.trace.total_ms;
        assert!((0.0..1_000.0).contains(&total), "{total}");
        for s in &outcome.trace.stages {
            assert!(
                s.ms >= 0.0 && s.ms <= total,
                "stage {} ms {} must sit within total {}",
                s.stage,
                s.ms,
                total
            );
        }
    }

    // ---- LadderOutcome::fold_fallback ----

    #[test]
    fn the_fallback_walk_starts_from_the_first_walks_refusals() {
        // walk 1 refused "abc" (its port content); walk 2 reads only a port
        // read of it there, and a plain detection — the refused one is not
        // published, the plain one replaces the empty walk 1
        let cancel = CancelToken::new();
        let place = [
            Point { x: 0.0, y: 0.0 },
            Point { x: 100.0, y: 0.0 },
            Point { x: 100.0, y: 100.0 },
            Point { x: 0.0, y: 100.0 },
        ];
        let mut walk1 = fresh_run(&cancel, (100, 100));
        walk1.store.refuse(
            engine::Refusal {
                port: Some(ContentKey {
                    raw: b"abc".to_vec(),
                    eci: EciEvidence::Signalled,
                    fnc1: Fnc1::None,
                }),
                rqrr: None,
                place: Some(place),
            },
            0,
        );
        let mut first = walk1.finish(Vec::new(), Instant::now()).unwrap();
        let mut walk2 = Run::new(&cancel, None, (100, 100), first.seed());
        let signalled = ContentKey {
            raw: b"abc".to_vec(),
            eci: EciEvidence::Signalled,
            fnc1: Fnc1::None,
        };
        let plain = ContentKey {
            raw: b"hello".to_vec(),
            eci: EciEvidence::NoEci,
            fnc1: Fnc1::None,
        };
        let rank = |index| Rank {
            attempt: 0,
            source: 0,
            index,
        };
        let at = Some([Point { x: 50.0, y: 50.0 }; 4]);
        walk2.store.collect(
            Read::new(
                Symbology::QrCode,
                signalled.clone(),
                Some(EngineKind::Rxing),
                rank(0),
            )
            .at(at),
        );
        walk2.store.collect(Read::new(
            Symbology::QrCode,
            plain,
            Some(EngineKind::Rxing),
            rank(1),
        ));
        let second = walk2.finish(Vec::new(), Instant::now()).unwrap();
        assert_eq!(
            second.merged.len(),
            1,
            "the seed withholds the refused read"
        );
        assert!(first.fold_fallback(second));
        assert_eq!(first.merged.len(), 1);
        assert_eq!(first.merged[0].raw, b"hello");
        assert_eq!(first.contents[0].0.raw, b"hello", "aligned");
        // a walk-2 parse protecting that content keeps it: walk 2's own
        // seeded protection
        let mut walk1 = fresh_run(&cancel, (100, 100));
        walk1.store.refuse(
            engine::Refusal {
                port: Some(signalled.clone()),
                rqrr: None,
                place: None,
            },
            0,
        );
        let mut first = walk1.finish(Vec::new(), Instant::now()).unwrap();
        let mut walk2 = Run::new(&cancel, None, (100, 100), first.seed());
        walk2.store.collect(Read::new(
            Symbology::QrCode,
            signalled,
            Some(EngineKind::Rxing),
            rank(0),
        ));
        let mut located = Read::new(
            Symbology::QrCode,
            ContentKey {
                raw: b"abc".to_vec(),
                eci: eci_mark(26, 0),
                fnc1: Fnc1::None,
            },
            Some(EngineKind::Rqrr),
            rank(1),
        );
        located.parsed = true;
        walk2.store.collect(located);
        let second = walk2.finish(Vec::new(), Instant::now()).unwrap();
        assert!(first.fold_fallback(second));
        assert_eq!(
            first.merged[0].engines,
            vec![EngineKind::Rxing, EngineKind::Rqrr]
        );
    }

    #[test]
    fn a_fallback_read_made_outside_the_refused_symbol_stands() {
        // walk 1 refused "abc" at A; walk 2 reads it at A first, then at B
        // outside it — one folded read, which stands there. Folding the
        // fallback walk in keeps it: walk 2 already applied walk 1's
        // refusals and protection, place by place, so the fold needs no
        // guard of its own
        let cancel = CancelToken::new();
        let signalled = ContentKey {
            raw: b"abc".to_vec(),
            eci: EciEvidence::Signalled,
            fnc1: Fnc1::None,
        };
        let mut walk1 = fresh_run(&cancel, (400, 100));
        walk1.store.refuse(
            engine::Refusal {
                port: Some(signalled.clone()),
                rqrr: None,
                place: Some(square_at(0.0)),
            },
            0,
        );
        let mut first = walk1.finish(Vec::new(), Instant::now()).unwrap();
        assert!(first.merged.is_empty());
        let mut walk2 = Run::new(&cancel, None, (400, 100), first.seed());
        for (attempt, x) in [(0, 1.0), (1, 250.0)] {
            let rank = Rank {
                attempt,
                source: source::RXING,
                index: 0,
            };
            walk2.store.collect(
                Read::new(
                    Symbology::QrCode,
                    signalled.clone(),
                    Some(EngineKind::Rxing),
                    rank,
                )
                .at(Some(square_at(x))),
            );
        }
        let second = walk2.finish(Vec::new(), Instant::now()).unwrap();
        assert_eq!(second.merged.len(), 1, "walk 2 publishes it");
        assert!(first.fold_fallback(second), "and the fold keeps it");
        assert_eq!(first.merged.len(), 1);
        assert_eq!(first.contents[0].0, signalled);
    }

    // ---- deep_attempts ----

    #[test]
    fn deep_attempts_full_res_grid_uses_the_stretch_variant() {
        // delete `!`. The (size=None, stretch=false) grid combo is SKIPPED
        // (it duplicates the S3 otsu/invert at full res); only (None,
        // stretch=true) survives. The lone inverting full-res grid attempt
        // must therefore build invert(contrast_stretch(luma)).
        let side = 64u32;
        let mut data = vec![0u8; (side * side) as usize];
        for (i, px) in data.iter_mut().enumerate() {
            *px = if i % 2 == 0 { 60 } else { 190 }; // bimodal, NOT full range
        }
        let luma = LumaImage::new(data, side, side);
        let attempts = deep_attempts(&luma, side);
        let inverting: Vec<&Attempt<'_>> = attempts.iter().filter(|a| a.inverts).collect();
        assert_eq!(
            inverting.len(),
            1,
            "one inverting full-res grid combo survives"
        );
        let got = (inverting[0].build)();
        let expected = transform::invert(&transform::contrast_stretch(&luma));
        assert_eq!(got.data(), expected.data());
    }

    // ---- run: stage gating + early exit ----

    #[test]
    fn pyramid_skipped_when_image_not_larger_than_pyramid_side() {
        // `>` -> `==`/`>=`. longest == pyramid_side means downscaling to
        // pyramid_side is a no-op, so the strict `>` skips the pyramid.
        let data = vec![255u8; 128 * 128];
        let planes = normalize(&ImageInput::luma8(&data, 128, 128), &Limits::default()).unwrap();
        let mut config = ScanConfig::full();
        config.pyramid_side = 128; // == longest
        config.budget_ms = None;
        let outcome = run(&planes, &config, &CancelToken::new(), None).unwrap();
        assert!(
            !stage_names(&outcome).contains(&"pyramid"),
            "{:?}",
            stage_names(&outcome)
        );
    }

    #[test]
    fn pyramid_runs_when_image_exceeds_pyramid_side() {
        // delete `!` before `out_of_budget()`: with an available budget the
        // pyramid MUST run on an oversized image.
        let data = vec![255u8; 700 * 700];
        let planes = normalize(&ImageInput::luma8(&data, 700, 700), &Limits::default()).unwrap();
        let mut config = ScanConfig::full(); // pyramid_side = 512
        config.budget_ms = None;
        let outcome = run(&planes, &config, &CancelToken::new(), None).unwrap();
        assert_eq!(stage_names(&outcome).first().copied(), Some("pyramid"));
    }

    #[test]
    fn ladder_stops_after_pyramid_finds_a_symbol() {
        // `stop || settled()` -> `&&`. A symbol decoded at the pyramid
        // downscale must break the ladder before direct.
        let bytes = qr_png("https://qrcode-ai.com/pyr", 24); // > 512px longest
        let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
        let mut config = ScanConfig::full();
        config.budget_ms = None;
        let outcome = run(&planes, &config, &CancelToken::new(), None).unwrap();
        assert_eq!(outcome.merged.len(), 1, "pyramid decodes the symbol");
        assert!(
            !stage_names(&outcome).contains(&"direct"),
            "{:?}",
            stage_names(&outcome)
        );
    }

    #[test]
    fn ladder_stops_after_direct_finds_a_symbol() {
        let bytes = qr_png("https://qrcode-ai.com/direct-stop-pin", 6); // < 512px
        let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
        let mut config = ScanConfig::full();
        config.budget_ms = None;
        let outcome = run(&planes, &config, &CancelToken::new(), None).unwrap();
        assert_eq!(outcome.merged.len(), 1);
        assert!(
            !stage_names(&outcome).contains(&"pyramid"),
            "{:?}",
            stage_names(&outcome)
        );
        assert!(
            !stage_names(&outcome).contains(&"enhance"),
            "{:?}",
            stage_names(&outcome)
        );
    }

    #[test]
    fn enhance_stage_is_gated_by_its_config_flag() {
        let data = vec![255u8; 64 * 64];
        let planes = normalize(&ImageInput::luma8(&data, 64, 64), &Limits::default()).unwrap();
        let mut config = ScanConfig::full();
        config.enhance = false;
        config.budget_ms = None;
        let outcome = run(&planes, &config, &CancelToken::new(), None).unwrap();
        assert!(
            !stage_names(&outcome).contains(&"enhance"),
            "{:?}",
            stage_names(&outcome)
        );
    }

    #[test]
    fn ladder_stops_after_enhance_finds_a_symbol() {
        let bytes = qr_png("https://qrcode-ai.com/enhance-stop-pin", 6);
        let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
        let mut config = ScanConfig::full();
        config.pyramid = false;
        config.direct = false;
        config.budget_ms = None;
        let outcome = run(&planes, &config, &CancelToken::new(), None).unwrap();
        assert_eq!(outcome.merged.len(), 1, "enhance decodes the symbol");
        assert!(
            !stage_names(&outcome).contains(&"deep"),
            "{:?}",
            stage_names(&outcome)
        );
    }

    #[test]
    fn deep_stage_is_gated_by_its_config_flag() {
        let data = vec![255u8; 64 * 64];
        let planes = normalize(&ImageInput::luma8(&data, 64, 64), &Limits::default()).unwrap();
        let mut config = ScanConfig::full();
        config.deep = false;
        config.budget_ms = None;
        let outcome = run(&planes, &config, &CancelToken::new(), None).unwrap();
        assert!(
            !stage_names(&outcome).contains(&"deep"),
            "{:?}",
            stage_names(&outcome)
        );
    }
}
