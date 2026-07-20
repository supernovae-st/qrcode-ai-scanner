//! Python bindings for `qrcode-ai-scanner` (PyO3).
//!
//! Thin wrapper over the Rust core: bytes → scan → the same versioned `ScanReport`,
//! returned as a native Python `dict` (serialized via the core's serde contract, the
//! cross-surface SSOT in `spec/`). The scan runs with the GIL released so multi-threaded
//! Python servers aren't blocked for the scan's wall-clock budget.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use scanner_core::{
    AlphaBackground, ImageInput, Limits, ScanProfile, Scanner, ScoreCheck, StressAxis,
};

fn parse_profile(profile: &str) -> PyResult<ScanProfile> {
    // Delegate to the library's canonical parser (same path as the Node/WASM
    // bindings) so all surfaces stay in sync as profiles evolve.
    ScanProfile::from_name(profile).ok_or_else(|| {
        PyValueError::new_err(format!(
            "unknown profile {profile:?} (expected 'full', 'fast', or 'frame')"
        ))
    })
}

/// Optional input caps from the caller (raise for huge images, lower to harden a
/// server against adversarial input). `None` unless at least one cap is set; any
/// unspecified field keeps the library default.
fn build_limits(max_dimension: Option<u32>, max_pixels: Option<u64>) -> Option<Limits> {
    if max_dimension.is_none() && max_pixels.is_none() {
        return None;
    }
    let d = Limits::default();
    Some(Limits {
        max_dimension: max_dimension.unwrap_or(d.max_dimension),
        max_pixels: max_pixels.unwrap_or(d.max_pixels),
    })
}

/// `budget_ms` overrides the profile's wall-clock budget (0 = unbounded, NOT a
/// zero-millisecond budget — spec/02): server embedders bound tail latency
/// without giving up the deep ladder. `score_skip_axes` (wire names) excludes
/// axes from scoring engine-side — the generated-preview integration config
/// (spec/04 § skipping axes); unknown names raise, never silently score six.
fn with_config(
    profile: ScanProfile,
    budget_ms: Option<u64>,
    score_skip_axes: Option<Vec<String>>,
    score_skip_checks: Option<Vec<String>>,
    alpha_background: Option<String>,
    alpha_palette: Option<Vec<String>>,
) -> PyResult<ScanProfile> {
    let skip: Vec<StressAxis> = match &score_skip_axes {
        None => Vec::new(),
        Some(names) => names
            .iter()
            .map(|n| {
                StressAxis::from_name(n).ok_or_else(|| {
                    PyValueError::new_err(format!(
                        "unknown stress axis `{n}` — expected resolution | blur | \
                         contrast | perspective | rotation | lighting"
                    ))
                })
            })
            .collect::<PyResult<_>>()?,
    };
    // score_skip_checks: report SECTIONS excluded at the source (`uec` /
    // `iso15415`) — never computed, wire carries null, UEC-driven hints
    // silent, composite untouched (spec/04 § skipping checks).
    let checks: Vec<ScoreCheck> = match &score_skip_checks {
        None => Vec::new(),
        Some(names) => names
            .iter()
            .map(|n| {
                ScoreCheck::from_name(n).ok_or_else(|| {
                    PyValueError::new_err(format!(
                        "unknown score check `{n}` — expected uec | iso15415 | alpha_envelope"
                    ))
                })
            })
            .collect::<PyResult<_>>()?,
    };
    // alpha_background: the flatten under transparent pixels (`auto` |
    // `white` | `black` | `none` | `#rrggbb`) — engine-side, uniform
    // across bindings; opaque inputs untouched (spec/01 § alpha).
    let alpha = match &alpha_background {
        None => None,
        Some(name) => Some(AlphaBackground::from_name(name).ok_or_else(|| {
            PyValueError::new_err(format!(
                "unknown alpha background `{name}` — expected auto | white | black | none | #rrggbb"
            ))
        })?),
    };
    // alpha_palette: HOST theme/brand colors probed by the envelope in one
    // scan (`alpha.envelope.palette`) — same loud posture on a bad color.
    let palette: Vec<[u8; 3]> = match &alpha_palette {
        None => Vec::new(),
        Some(names) => names
            .iter()
            .map(|n| {
                AlphaBackground::palette_color(n).ok_or_else(|| {
                    PyValueError::new_err(format!(
                        "unknown palette color `{n}` — expected white | black | #rrggbb"
                    ))
                })
            })
            .collect::<PyResult<_>>()?,
    };
    if budget_ms.is_none()
        && skip.is_empty()
        && checks.is_empty()
        && alpha.is_none()
        && palette.is_empty()
    {
        return Ok(profile);
    }
    let mut config = profile.config();
    if let Some(ms) = budget_ms {
        config.budget_ms = (ms > 0).then_some(ms);
    }
    config.score_skip_axes = skip;
    config.score_skip_checks = checks;
    if let Some(alpha) = alpha {
        config.alpha_background = alpha;
    }
    config.alpha_palette = palette;
    Ok(ScanProfile::Custom(config))
}

// Serialize a ScanReport to a Python object via serde_json::Value so Rust tuples /
// [T; N] become JSON arrays → idiomatic Python lists, conforming to spec/'s schema.
fn report_to_py<'py>(
    py: Python<'py>,
    report: &scanner_core::ScanReport,
) -> PyResult<Bound<'py, PyAny>> {
    let value = serde_json::to_value(report).map_err(|e| PyValueError::new_err(e.to_string()))?;
    pythonize::pythonize(py, &value).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// Decode + score an encoded image (PNG · JPEG · WebP · GIF).
///
/// Returns the `ScanReport` as a `dict`. "No QR found" is a normal result (empty
/// `detections`); a `ValueError` is raised only for invalid input or cancellation.
/// `budget_ms` overrides the profile's wall-clock budget (0 = unbounded).
#[pyfunction]
#[pyo3(signature = (image, profile = "full", max_dimension = None, max_pixels = None, budget_ms = None, score_skip_axes = None, score_skip_checks = None, alpha_background = None, alpha_palette = None))]
#[expect(
    clippy::too_many_arguments,
    reason = "the Python signature IS the cross-binding contract (profile + caps + budget + skips + alpha)"
)]
fn scan<'py>(
    py: Python<'py>,
    image: &[u8],
    profile: &str,
    max_dimension: Option<u32>,
    max_pixels: Option<u64>,
    budget_ms: Option<u64>,
    score_skip_axes: Option<Vec<String>>,
    score_skip_checks: Option<Vec<String>>,
    alpha_background: Option<String>,
    alpha_palette: Option<Vec<String>>,
) -> PyResult<Bound<'py, PyAny>> {
    let profile = with_config(
        parse_profile(profile)?,
        budget_ms,
        score_skip_axes,
        score_skip_checks,
        alpha_background,
        alpha_palette,
    )?;
    let limits = build_limits(max_dimension, max_pixels);
    let image = image.to_vec(); // own it before releasing the GIL
    let report = py
        .detach(move || {
            let builder = Scanner::builder().profile(profile);
            let builder = match limits {
                Some(l) => builder.limits(l),
                None => builder,
            };
            builder.build().scan(ImageInput::encoded(&image))
        })
        .map_err(|e| PyValueError::new_err(format!("{} [{}]", e, e.code())))?;
    report_to_py(py, &report)
}

/// Decode + score a raw RGBA frame (e.g. a camera frame), no image-format roundtrip.
///
/// `rgba` must be `width * height * 4` bytes. `budget_ms` overrides the profile's
/// wall-clock budget (0 = unbounded) — the camera loop's per-frame bound.
#[pyfunction]
#[pyo3(signature = (rgba, width, height, profile = "frame", max_dimension = None, max_pixels = None, budget_ms = None, score_skip_axes = None, score_skip_checks = None, alpha_background = None, alpha_palette = None))]
#[expect(
    clippy::too_many_arguments,
    reason = "the Python signature IS the cross-binding contract (dims + profile + caps + budget + skips + alpha)"
)]
fn scan_frame<'py>(
    py: Python<'py>,
    rgba: &[u8],
    width: u32,
    height: u32,
    profile: &str,
    max_dimension: Option<u32>,
    max_pixels: Option<u64>,
    budget_ms: Option<u64>,
    score_skip_axes: Option<Vec<String>>,
    score_skip_checks: Option<Vec<String>>,
    alpha_background: Option<String>,
    alpha_palette: Option<Vec<String>>,
) -> PyResult<Bound<'py, PyAny>> {
    let profile = with_config(
        parse_profile(profile)?,
        budget_ms,
        score_skip_axes,
        score_skip_checks,
        alpha_background,
        alpha_palette,
    )?;
    let limits = build_limits(max_dimension, max_pixels);
    let rgba = rgba.to_vec(); // own it before releasing the GIL
    let report = py
        .detach(move || {
            let builder = Scanner::builder().profile(profile);
            let builder = match limits {
                Some(l) => builder.limits(l),
                None => builder,
            };
            builder
                .build()
                .scan(ImageInput::rgba8(&rgba, width, height))
        })
        .map_err(|e| PyValueError::new_err(format!("{} [{}]", e, e.code())))?;
    report_to_py(py, &report)
}

#[pymodule]
fn qrcode_ai_scanner(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("__all__", vec!["scan", "scan_frame", "__version__"])?;
    m.add_function(wrap_pyfunction!(scan, m)?)?;
    m.add_function(wrap_pyfunction!(scan_frame, m)?)?;
    Ok(())
}
