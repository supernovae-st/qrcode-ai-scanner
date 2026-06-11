//! Deterministic preprocessing — pure pixel ops + source planes access.
//!
//! Every function here is a pure deterministic mapping (no RNG, no wall
//! clock): same input ⇒ same output, the property the whole ladder and the
//! score contract stand on. RGB access is LAZY: channels materialize only
//! when the enhance stage actually runs (artistic-miss path).

use crate::error::{Result, ScanError};
use crate::input::{ImageInput, Limits, LumaImage, bt601, validate_buffer, validate_dims};

/// Color channel selector for per-channel decode attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channel {
    /// Red plane.
    R,
    /// Green plane.
    G,
    /// Blue plane.
    B,
}

/// Normalized scan source: the luma plane always, RGB retained when the
/// input had color (channel planes materialize lazily on demand).
#[derive(Debug, Clone)]
pub(crate) struct SourcePlanes {
    /// BT.601 luma plane — what stages S1/S2 consume.
    pub luma: LumaImage,
    /// Interleaved RGB (w*h*3) when the source carried color.
    rgb: Option<Vec<u8>>,
}

impl SourcePlanes {
    /// Materialize one color channel as a luma plane. `None` for grayscale sources.
    pub(crate) fn channel(&self, channel: Channel) -> Option<LumaImage> {
        let rgb = self.rgb.as_ref()?;
        let offset = match channel {
            Channel::R => 0,
            Channel::G => 1,
            Channel::B => 2,
        };
        let data: Vec<u8> = rgb[offset..].iter().step_by(3).copied().collect();
        Some(LumaImage::new(data, self.luma.width(), self.luma.height()))
    }
}

/// Validate an [`ImageInput`] and normalize it into scan planes.
///
/// The single decode point of the crate: encoded bytes are dimension-probed
/// BEFORE full decode (decompression-bomb guard), raw buffers are
/// length-checked, and color is retained for the enhance stage.
pub(crate) fn normalize(input: &ImageInput<'_>, limits: &Limits) -> Result<SourcePlanes> {
    match *input {
        ImageInput::Luma8 {
            data,
            width,
            height,
        } => {
            let pixels = validate_dims(width, height, limits)?;
            validate_buffer(data.len(), pixels)?;
            Ok(SourcePlanes {
                luma: LumaImage::new(data.to_vec(), width, height),
                rgb: None,
            })
        }
        ImageInput::Rgba8 {
            data,
            width,
            height,
        } => {
            let pixels = validate_dims(width, height, limits)?;
            validate_buffer(data.len(), pixels * 4)?;
            let mut luma = Vec::with_capacity(data.len() / 4);
            let mut rgb = Vec::with_capacity(data.len() / 4 * 3);
            for px in data.chunks_exact(4) {
                luma.push(bt601(px[0], px[1], px[2]));
                rgb.extend_from_slice(&px[..3]);
            }
            Ok(SourcePlanes {
                luma: LumaImage::new(luma, width, height),
                rgb: Some(rgb),
            })
        }
        ImageInput::Encoded(bytes) => {
            let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
                .with_guessed_format()
                .map_err(|e| ScanError::InvalidImage {
                    details: e.to_string(),
                })?;
            let (width, height) =
                reader
                    .into_dimensions()
                    .map_err(|e| ScanError::InvalidImage {
                        details: e.to_string(),
                    })?;
            validate_dims(width, height, limits)?;
            let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
                .with_guessed_format()
                .map_err(|e| ScanError::InvalidImage {
                    details: e.to_string(),
                })?;
            // second wall behind the dimension probe: the decoder itself
            // refuses allocations past our pixel budget (malformed headers)
            let mut decode_limits = image::Limits::default();
            decode_limits.max_image_width = Some(limits.max_dimension);
            decode_limits.max_image_height = Some(limits.max_dimension);
            decode_limits.max_alloc = Some(limits.max_pixels.saturating_mul(8));
            reader.limits(decode_limits);
            let img = reader.decode().map_err(|e| ScanError::InvalidImage {
                details: e.to_string(),
            })?;
            let rgb = img.color().has_color().then(|| img.to_rgb8().into_raw());
            let luma = img.into_luma8();
            let (w, h) = (luma.width(), luma.height());
            Ok(SourcePlanes {
                luma: LumaImage::new(luma.into_raw(), w, h),
                rgb,
            })
        }
    }
}

/// Otsu binarization: maximize between-class variance, map `luma > t → 255`.
#[expect(
    clippy::cast_precision_loss,
    reason = "counts/sums are bounded by Limits::max_pixels (64MP·255 < 2^52) — exact in f64"
)]
#[expect(
    clippy::needless_range_loop,
    reason = "t is simultaneously the histogram index AND the candidate threshold value"
)]
pub(crate) fn otsu_threshold(img: &LumaImage) -> LumaImage {
    let mut histogram = [0u64; 256];
    for &px in img.data() {
        histogram[px as usize] += 1;
    }
    let total: u64 = img.data().len() as u64;
    let weighted_sum: u64 = histogram
        .iter()
        .enumerate()
        .map(|(value, &count)| value as u64 * count)
        .sum();

    let mut best_threshold = 0usize;
    let mut best_variance = -1.0f64;
    let mut background_count = 0u64;
    let mut background_sum = 0u64;
    for t in 0..256 {
        background_count += histogram[t];
        if background_count == 0 {
            continue;
        }
        let foreground_count = total - background_count;
        if foreground_count == 0 {
            break;
        }
        background_sum += t as u64 * histogram[t];
        let background_mean = background_sum as f64 / background_count as f64;
        let foreground_mean = (weighted_sum - background_sum) as f64 / foreground_count as f64;
        let separation = background_mean - foreground_mean;
        let variance = background_count as f64 * foreground_count as f64 * separation * separation;
        if variance > best_variance {
            best_variance = variance;
            best_threshold = t;
        }
    }

    let threshold = u8::try_from(best_threshold).unwrap_or(u8::MAX);
    let data = img
        .data()
        .iter()
        .map(|&px| if px > threshold { 255 } else { 0 })
        .collect();
    LumaImage::new(data, img.width(), img.height())
}

/// Exact photometric inversion.
pub(crate) fn invert(img: &LumaImage) -> LumaImage {
    let data = img.data().iter().map(|&px| 255 - px).collect();
    LumaImage::new(data, img.width(), img.height())
}

/// Min-max contrast stretch to the full 0-255 range (flat images unchanged).
pub(crate) fn contrast_stretch(img: &LumaImage) -> LumaImage {
    let Some(&min) = img.data().iter().min() else {
        return img.clone();
    };
    let Some(&max) = img.data().iter().max() else {
        return img.clone();
    };
    if min == max {
        return img.clone();
    }
    let range = u32::from(max - min);
    let data = img
        .data()
        .iter()
        .map(|&px| {
            let stretched = u32::from(px - min) * 255 / range;
            u8::try_from(stretched).unwrap_or(u8::MAX)
        })
        .collect();
    LumaImage::new(data, img.width(), img.height())
}

/// Deterministic box-average downscale so the longest side fits `max_side`.
/// No-op (clone) when the image already fits.
#[expect(
    clippy::cast_possible_truncation,
    reason = "every u64 here is a pixel index bounded by the u32 source dimensions"
)]
pub(crate) fn downscale_to(img: &LumaImage, max_side: u32) -> LumaImage {
    let (w, h) = (img.width(), img.height());
    let longest = w.max(h);
    if longest <= max_side {
        return img.clone();
    }
    let new_w = (u64::from(w) * u64::from(max_side) / u64::from(longest)).max(1) as u32;
    let new_h = (u64::from(h) * u64::from(max_side) / u64::from(longest)).max(1) as u32;

    let src = img.data();
    let mut data = Vec::with_capacity((new_w * new_h) as usize);
    for oy in 0..new_h {
        let y0 = (u64::from(oy) * u64::from(h) / u64::from(new_h)) as u32;
        let y1 =
            ((u64::from(oy) + 1) * u64::from(h) / u64::from(new_h)).max(u64::from(y0) + 1) as u32;
        for ox in 0..new_w {
            let x0 = (u64::from(ox) * u64::from(w) / u64::from(new_w)) as u32;
            let x1 = ((u64::from(ox) + 1) * u64::from(w) / u64::from(new_w)).max(u64::from(x0) + 1)
                as u32;
            let mut sum = 0u64;
            for y in y0..y1 {
                let row = (y * w) as usize;
                for x in x0..x1 {
                    sum += u64::from(src[row + x as usize]);
                }
            }
            let count = u64::from(y1 - y0) * u64::from(x1 - x0);
            data.push(u8::try_from(sum / count).unwrap_or(u8::MAX));
        }
    }
    LumaImage::new(data, new_w, new_h)
}

/// v0.2-semantics contrast/brightness boost:
/// `clamp(((p · brightness) − 128) · contrast + 128)`.
/// Multiplicative contrast around the midpoint — distinct from
/// [`contrast_stretch`] (min-max): boost CRUSHES mid-tones outward, which is
/// what separates art texture from module structure on artistic codes.
pub(crate) fn contrast_boost(img: &LumaImage, contrast: f32, brightness: f32) -> LumaImage {
    let data = img
        .data()
        .iter()
        .map(|&px| {
            let adjusted = ((f32::from(px) * brightness) - 128.0) * contrast + 128.0;
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 0.0..=255.0 on the previous line"
            )]
            {
                adjusted.clamp(0.0, 255.0) as u8
            }
        })
        .collect();
    LumaImage::new(data, img.width(), img.height())
}

/// Deterministic gaussian blur (separable, via the image crate).
/// On artistic codes a light blur averages art texture into module means.
pub(crate) fn gaussian_blur(img: &LumaImage, sigma: f32) -> LumaImage {
    let Some(buffer) = image::ImageBuffer::<image::Luma<u8>, _>::from_raw(
        img.width(),
        img.height(),
        img.data().to_vec(),
    ) else {
        return img.clone();
    };
    let blurred = image::imageops::blur(&buffer, sigma);
    let (w, h) = (blurred.width(), blurred.height());
    LumaImage::new(blurred.into_raw(), w, h)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::input::{ImageInput, Limits};

    fn luma(data: &[u8], w: u32, h: u32) -> LumaImage {
        LumaImage::new(data.to_vec(), w, h)
    }

    #[test]
    fn otsu_splits_a_bimodal_image_exactly() {
        let img = luma(&[10, 10, 200, 200], 2, 2);
        let out = otsu_threshold(&img);
        assert_eq!(out.data(), &[0, 0, 255, 255]);
    }

    #[test]
    fn otsu_flat_image_maps_to_white() {
        // Convention pinned: luma > otsu-threshold → 255 (flat lands all-white).
        let img = luma(&[128; 9], 3, 3);
        let out = otsu_threshold(&img);
        assert_eq!(out.data(), &[255; 9]);
    }

    #[test]
    fn invert_is_exact() {
        let img = luma(&[0, 128, 255], 3, 1);
        let out = invert(&img);
        assert_eq!(out.data(), &[255, 127, 0]);
    }

    #[test]
    fn contrast_stretch_min_max_full_range() {
        let img = luma(&[100, 150, 125], 3, 1);
        let out = contrast_stretch(&img);
        assert_eq!(out.data(), &[0, 255, 127]);
    }

    #[test]
    fn contrast_stretch_flat_image_unchanged() {
        let img = luma(&[7, 7, 7, 7], 2, 2);
        let out = contrast_stretch(&img);
        assert_eq!(out.data(), &[7, 7, 7, 7]);
    }

    #[test]
    fn downscale_averages_blocks_exactly() {
        // 4x4 with 2x2 uniform blocks of 0 / 40 / 200 / 100.
        #[rustfmt::skip]
        let img = luma(&[
            0, 0, 40, 40,
            0, 0, 40, 40,
            200, 200, 100, 100,
            200, 200, 100, 100,
        ], 4, 4);
        let out = downscale_to(&img, 2);
        assert_eq!((out.width(), out.height()), (2, 2));
        assert_eq!(out.data(), &[0, 40, 200, 100]);
    }

    #[test]
    fn downscale_noop_when_already_small() {
        let img = luma(&[1, 2, 3, 4], 2, 2);
        let out = downscale_to(&img, 512);
        assert_eq!(out.data(), img.data());
        assert_eq!((out.width(), out.height()), (2, 2));
    }

    #[test]
    fn contrast_boost_matches_v02_semantics() {
        let img = luma(&[100, 200, 128], 3, 1);
        let out = contrast_boost(&img, 2.0, 1.0);
        // ((100-128)*2)+128 = 72 · ((200-128)*2)+128 = 272→255 · 128 fixed point
        assert_eq!(out.data(), &[72, 255, 128]);
    }

    #[test]
    fn contrast_boost_brightness_applies_before_contrast() {
        let img = luma(&[100], 1, 1);
        let out = contrast_boost(&img, 2.0, 1.1);
        // ((100*1.1)-128)*2+128 = 92
        assert_eq!(out.data(), &[92]);
    }

    #[test]
    fn gaussian_blur_spreads_an_impulse_deterministically() {
        let mut data = vec![0u8; 25];
        data[12] = 255;
        let img = luma(&data, 5, 5);
        let a = gaussian_blur(&img, 1.0);
        let b = gaussian_blur(&img, 1.0);
        assert_eq!(a.data(), b.data(), "blur must be deterministic");
        assert!(a.data()[12] < 255, "center spread out");
        assert!(
            a.data()[11] > 0 && a.data()[7] > 0,
            "neighbors received mass"
        );
        assert_eq!((a.width(), a.height()), (5, 5));
    }

    #[test]
    fn rgba_source_extracts_channels() {
        // red px, green px
        let rgba = [255u8, 0, 0, 255, 0, 255, 0, 255];
        let planes = normalize(&ImageInput::rgba8(&rgba, 2, 1), &Limits::default()).unwrap();
        let r = planes.channel(Channel::R).expect("rgb retained");
        assert_eq!(r.data(), &[255, 0]);
        let g = planes.channel(Channel::G).expect("rgb retained");
        assert_eq!(g.data(), &[0, 255]);
    }

    #[test]
    fn encoded_color_source_extracts_channels_lazily() {
        // 2x1 png: pure blue then pure red
        let mut img = image::RgbImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgb([0, 0, 255]));
        img.put_pixel(1, 0, image::Rgb([255, 0, 0]));
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();

        let planes = normalize(&ImageInput::encoded(&buf), &Limits::default()).unwrap();
        let b = planes.channel(Channel::B).expect("color png has channels");
        assert_eq!(b.data(), &[255, 0]);
    }

    #[test]
    fn luma_source_has_no_channels() {
        let data = [0u8, 255];
        let planes = normalize(&ImageInput::luma8(&data, 2, 1), &Limits::default()).unwrap();
        assert!(planes.channel(Channel::R).is_none());
        assert_eq!(planes.luma.data(), &[0, 255]);
    }
}
