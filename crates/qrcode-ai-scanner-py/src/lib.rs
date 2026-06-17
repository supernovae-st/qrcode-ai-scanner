//! Python bindings for `qrcode-ai-scanner` (PyO3).
//!
//! Thin wrapper over the Rust core: bytes → scan → the same versioned `ScanReport`,
//! returned as a native Python `dict` (serialized via the core's serde contract, the
//! cross-surface SSOT in `spec/`). The scan runs with the GIL released so multi-threaded
//! Python servers aren't blocked for the scan's wall-clock budget.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use scanner_core::{ImageInput, ScanProfile, Scanner};

fn parse_profile(profile: &str) -> PyResult<ScanProfile> {
    // Delegate to the library's canonical parser (same path as the Node/WASM
    // bindings) so all surfaces stay in sync as profiles evolve.
    ScanProfile::from_name(profile).ok_or_else(|| {
        PyValueError::new_err(format!(
            "unknown profile {profile:?} (expected 'full', 'fast', or 'frame')"
        ))
    })
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
#[pyfunction]
#[pyo3(signature = (image, profile = "full"))]
fn scan<'py>(py: Python<'py>, image: &[u8], profile: &str) -> PyResult<Bound<'py, PyAny>> {
    let profile = parse_profile(profile)?;
    let image = image.to_vec(); // own it before releasing the GIL
    let report = py
        .detach(move || Scanner::builder().profile(profile).build().scan(ImageInput::encoded(&image)))
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    report_to_py(py, &report)
}

/// Decode + score a raw RGBA frame (e.g. a camera frame), no image-format roundtrip.
///
/// `rgba` must be `width * height * 4` bytes.
#[pyfunction]
#[pyo3(signature = (rgba, width, height, profile = "frame"))]
fn scan_frame<'py>(
    py: Python<'py>,
    rgba: &[u8],
    width: u32,
    height: u32,
    profile: &str,
) -> PyResult<Bound<'py, PyAny>> {
    let profile = parse_profile(profile)?;
    let rgba = rgba.to_vec(); // own it before releasing the GIL
    let report = py
        .detach(move || {
            Scanner::builder()
                .profile(profile)
                .build()
                .scan(ImageInput::rgba8(&rgba, width, height))
        })
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
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
