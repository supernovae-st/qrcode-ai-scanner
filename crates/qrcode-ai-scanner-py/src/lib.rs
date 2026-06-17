//! Python bindings for `qrcode-ai-scanner` (PyO3).
//!
//! Thin wrapper over the Rust core: bytes → scan → the same versioned `ScanReport`,
//! returned as a native Python `dict` (serialized via the core's serde contract, the
//! cross-surface SSOT in `spec/`).

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use scanner_core::{ImageInput, ScanProfile, Scanner};

fn parse_profile(profile: &str) -> PyResult<ScanProfile> {
    match profile.to_ascii_lowercase().as_str() {
        "full" => Ok(ScanProfile::Full),
        "fast" => Ok(ScanProfile::Fast),
        "frame" => Ok(ScanProfile::Frame),
        other => Err(PyValueError::new_err(format!(
            "unknown profile {other:?} (expected 'full', 'fast', or 'frame')"
        ))),
    }
}

/// Decode + score an encoded image (PNG · JPEG · WebP · GIF).
///
/// Returns the `ScanReport` as a `dict`. "No QR found" is a normal result (empty
/// `detections`); a `ValueError` is raised only for invalid input or cancellation.
#[pyfunction]
#[pyo3(signature = (image, profile = "full"))]
fn scan<'py>(py: Python<'py>, image: &[u8], profile: &str) -> PyResult<Bound<'py, PyAny>> {
    let scanner = Scanner::builder().profile(parse_profile(profile)?).build();
    let report = scanner
        .scan(ImageInput::encoded(image))
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    // Round-trip through serde_json::Value so Rust tuples / [T; N] become JSON
    // arrays → idiomatic Python lists (not tuples), conforming to spec/'s schema.
    let value = serde_json::to_value(&report).map_err(|e| PyValueError::new_err(e.to_string()))?;
    pythonize::pythonize(py, &value).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// Decode + score a raw RGBA frame (e.g. a camera frame), no image-format roundtrip.
#[pyfunction]
#[pyo3(signature = (rgba, width, height, profile = "frame"))]
fn scan_frame<'py>(
    py: Python<'py>,
    rgba: &[u8],
    width: u32,
    height: u32,
    profile: &str,
) -> PyResult<Bound<'py, PyAny>> {
    let scanner = Scanner::builder().profile(parse_profile(profile)?).build();
    let report = scanner
        .scan(ImageInput::rgba8(rgba, width, height))
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    // Round-trip through serde_json::Value so Rust tuples / [T; N] become JSON
    // arrays → idiomatic Python lists (not tuples), conforming to spec/'s schema.
    let value = serde_json::to_value(&report).map_err(|e| PyValueError::new_err(e.to_string()))?;
    pythonize::pythonize(py, &value).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[pymodule]
fn qrcode_ai_scanner(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(wrap_pyfunction!(scan, m)?)?;
    m.add_function(wrap_pyfunction!(scan_frame, m)?)?;
    Ok(())
}
