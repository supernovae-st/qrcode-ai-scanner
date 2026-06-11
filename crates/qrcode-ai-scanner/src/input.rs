//! Image input layer — borrowed inputs, anti-DoS limits, BT.601 luma.
//!
//! Every scan starts here: an [`ImageInput`] is validated against [`Limits`]
//! and normalized into the internal owned luma buffer the engines consume.
//! EXIF orientation is NOT applied in v0.3.0-alpha (open item — decoders are
//! rotation-tolerant; only corner coordinates are affected).

// Self-expiring: rustc flags this attribute as unfulfilled once the engine
// layer (A5+) consumes everything here — at which point it MUST be deleted.
// Scoped to non-test builds: the test target already uses every item.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "normalization layer lands before its consumers (engine layer, task A5)"
    )
)]

use crate::error::{Result, ScanError};

/// Borrowed image input for a scan.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum ImageInput<'a> {
    /// Encoded image bytes — PNG, JPEG, WebP or GIF.
    Encoded(&'a [u8]),
    /// Raw 8-bit RGBA pixels, row-major (browser `ImageData.data` shape).
    Rgba8 {
        /// Pixel bytes, `width * height * 4` long.
        data: &'a [u8],
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
    /// Raw 8-bit grayscale pixels, row-major.
    Luma8 {
        /// Pixel bytes, `width * height` long.
        data: &'a [u8],
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
}

impl<'a> ImageInput<'a> {
    /// Input from encoded image bytes (PNG/JPEG/WebP/GIF).
    #[must_use]
    pub fn encoded(bytes: &'a [u8]) -> Self {
        Self::Encoded(bytes)
    }

    /// Input from raw RGBA8 pixels (e.g. a camera frame's `ImageData`).
    #[must_use]
    pub fn rgba8(data: &'a [u8], width: u32, height: u32) -> Self {
        Self::Rgba8 {
            data,
            width,
            height,
        }
    }

    /// Input from raw grayscale pixels.
    #[must_use]
    pub fn luma8(data: &'a [u8], width: u32, height: u32) -> Self {
        Self::Luma8 {
            data,
            width,
            height,
        }
    }
}

/// Anti-DoS bounds applied to every input before any pixel work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum width or height in pixels.
    pub max_dimension: u32,
    /// Maximum total pixel count.
    pub max_pixels: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_dimension: 10_000,
            max_pixels: 64_000_000,
        }
    }
}

/// Internal owned grayscale image — the only shape engines consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LumaImage {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

impl LumaImage {
    pub(crate) fn new(data: Vec<u8>, width: u32, height: u32) -> Self {
        debug_assert_eq!(data.len() as u64, u64::from(width) * u64::from(height));
        Self {
            data,
            width,
            height,
        }
    }

    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    pub(crate) fn data(&self) -> &[u8] {
        &self.data
    }
}

/// Validate dimensions against limits. Zero dimensions are invalid input.
fn validate_dims(width: u32, height: u32, limits: &Limits) -> Result<u64> {
    if width == 0 || height == 0 {
        return Err(ScanError::InvalidImage {
            details: format!("zero dimension: {width}x{height}"),
        });
    }
    if width > limits.max_dimension || height > limits.max_dimension {
        return Err(ScanError::DimensionsExceeded {
            width,
            height,
            max_dimension: limits.max_dimension,
        });
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > limits.max_pixels {
        return Err(ScanError::PixelOverflow {
            width,
            height,
            max_pixels: limits.max_pixels,
        });
    }
    Ok(pixels)
}

/// Check a raw buffer length against the expected byte count.
fn validate_buffer(got: usize, expected: u64) -> Result<usize> {
    let expected_usize = usize::try_from(expected).map_err(|_| ScanError::PixelOverflow {
        width: 0,
        height: 0,
        max_pixels: expected,
    })?;
    if got != expected_usize {
        return Err(ScanError::BufferMismatch {
            got,
            expected: expected_usize,
        });
    }
    Ok(expected_usize)
}

/// BT.601 integer luma: `y = (299r + 587g + 114b + 500) / 1000`.
fn bt601(r: u8, g: u8, b: u8) -> u8 {
    let y = (299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b) + 500) / 1000;
    // 0..=255 by construction: max = (299+587+114)*255/1000 = 255
    u8::try_from(y).unwrap_or(u8::MAX)
}

/// Validate an [`ImageInput`] and normalize it into the internal luma buffer.
pub(crate) fn decode_to_luma(input: &ImageInput<'_>, limits: &Limits) -> Result<LumaImage> {
    match *input {
        ImageInput::Luma8 {
            data,
            width,
            height,
        } => {
            let pixels = validate_dims(width, height, limits)?;
            validate_buffer(data.len(), pixels)?;
            Ok(LumaImage::new(data.to_vec(), width, height))
        }
        ImageInput::Rgba8 {
            data,
            width,
            height,
        } => {
            let pixels = validate_dims(width, height, limits)?;
            validate_buffer(data.len(), pixels * 4)?;
            let luma: Vec<u8> = data
                .chunks_exact(4)
                .map(|px| bt601(px[0], px[1], px[2]))
                .collect();
            Ok(LumaImage::new(luma, width, height))
        }
        ImageInput::Encoded(bytes) => {
            // Header-only dimension probe BEFORE full decode (decompression-bomb guard).
            let (width, height) = image::ImageReader::new(std::io::Cursor::new(bytes))
                .with_guessed_format()
                .map_err(|e| ScanError::InvalidImage {
                    details: e.to_string(),
                })?
                .into_dimensions()
                .map_err(|e| ScanError::InvalidImage {
                    details: e.to_string(),
                })?;
            validate_dims(width, height, limits)?;
            let img = image::ImageReader::new(std::io::Cursor::new(bytes))
                .with_guessed_format()
                .map_err(|e| ScanError::InvalidImage {
                    details: e.to_string(),
                })?
                .decode()
                .map_err(|e| ScanError::InvalidImage {
                    details: e.to_string(),
                })?;
            let luma = img.into_luma8();
            let (w, h) = (luma.width(), luma.height());
            Ok(LumaImage::new(luma.into_raw(), w, h))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ScanError;

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let img = image::DynamicImage::ImageLuma8(image::ImageBuffer::new(width, height));
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn encoded_garbage_is_invalid_image() {
        let err = decode_to_luma(
            &ImageInput::encoded(b"definitely not an image"),
            &Limits::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ScanError::InvalidImage { .. }), "{err}");
        assert_eq!(err.code(), "QRS-001");
    }

    #[test]
    fn encoded_oversize_rejected_before_full_decode() {
        let bytes = png_bytes(2_001, 1);
        let limits = Limits {
            max_dimension: 2_000,
            ..Limits::default()
        };
        let err = decode_to_luma(&ImageInput::encoded(&bytes), &limits).unwrap_err();
        assert!(
            matches!(
                err,
                ScanError::DimensionsExceeded {
                    width: 2_001,
                    height: 1,
                    max_dimension: 2_000
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn raw_oversize_dimension_rejected() {
        let data = vec![0u8; 4];
        let err =
            decode_to_luma(&ImageInput::rgba8(&data, 10_001, 1), &Limits::default()).unwrap_err();
        assert_eq!(err.code(), "QRS-002");
    }

    #[test]
    fn pixel_cap_applies_before_buffer_check() {
        // 9000x9000 = 81MP > default 64MP cap — buffer length is irrelevant here.
        let err =
            decode_to_luma(&ImageInput::luma8(&[], 9_000, 9_000), &Limits::default()).unwrap_err();
        assert!(
            matches!(
                err,
                ScanError::PixelOverflow {
                    max_pixels: 64_000_000,
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn rgba8_buffer_mismatch_reports_expected_len() {
        let data = vec![0u8; 11];
        let err = decode_to_luma(&ImageInput::rgba8(&data, 2, 2), &Limits::default()).unwrap_err();
        assert!(
            matches!(
                err,
                ScanError::BufferMismatch {
                    got: 11,
                    expected: 16
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn luma8_buffer_mismatch_reports_expected_len() {
        let data = vec![0u8; 3];
        let err = decode_to_luma(&ImageInput::luma8(&data, 2, 2), &Limits::default()).unwrap_err();
        assert!(
            matches!(
                err,
                ScanError::BufferMismatch {
                    got: 3,
                    expected: 4
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn rgba8_converts_with_bt601_integer_coefficients() {
        // red · white · black · green
        let data: Vec<u8> = vec![
            255, 0, 0, 255, //
            255, 255, 255, 255, //
            0, 0, 0, 255, //
            0, 255, 0, 255,
        ];
        let luma = decode_to_luma(&ImageInput::rgba8(&data, 4, 1), &Limits::default()).unwrap();
        assert_eq!(luma.width(), 4);
        assert_eq!(luma.height(), 1);
        // y = (299r + 587g + 114b + 500) / 1000
        assert_eq!(luma.data(), &[76, 255, 0, 150]);
    }

    #[test]
    fn luma8_is_passthrough() {
        let data: Vec<u8> = vec![7, 42, 99, 200, 13, 0];
        let luma = decode_to_luma(&ImageInput::luma8(&data, 3, 2), &Limits::default()).unwrap();
        assert_eq!(luma.data(), data.as_slice());
        assert_eq!((luma.width(), luma.height()), (3, 2));
    }

    #[test]
    fn encoded_png_roundtrip_decodes_with_correct_dims() {
        let bytes = png_bytes(31, 17);
        let luma = decode_to_luma(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
        assert_eq!((luma.width(), luma.height()), (31, 17));
        assert_eq!(luma.data().len(), 31 * 17);
    }

    #[test]
    fn zero_dimension_is_invalid() {
        let err = decode_to_luma(&ImageInput::luma8(&[], 0, 4), &Limits::default()).unwrap_err();
        assert_eq!(err.code(), "QRS-001");
    }
}
