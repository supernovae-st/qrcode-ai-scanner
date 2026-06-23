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
    /// Whether the source carried color (channel planes available) —
    /// presence check only, no extraction.
    pub(crate) fn has_color(&self) -> bool {
        self.rgb.is_some()
    }

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
    // Fused min/max in ONE pass (was two separate `.min()`/`.max()` scans) —
    // identical (min, max) result, half the read traffic before the map pass.
    let Some((&first, rest)) = img.data().split_first() else {
        return img.clone(); // empty image: nothing to stretch
    };
    let (mut min, mut max) = (first, first);
    for &px in rest {
        min = min.min(px);
        max = max.max(px);
    }
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

/// Separable windowed-minimum filter (grayscale erosion). Dark structures
/// GROW by `(k-1)/2` pixels in every direction — on blob/dot pixel styles
/// this closes the gaps between sub-module blobs so grid sampling reads
/// solid modules instead of gap-noise. `k` is the full window side; even
/// values are bumped to the next odd. Output is identical to the naive
/// `O(w·h·k)` fold; the sliding-deque core makes it `O(w·h)` regardless of
/// window size (a single value-pinned test guards the two-impl equivalence).
pub(crate) fn min_filter(img: &LumaImage, k: u32) -> LumaImage {
    morph_filter(img, k, MorphPick::Min)
}

/// Separable windowed-maximum filter (grayscale dilation) — the mirror of
/// [`min_filter`] for LIGHT blob structures (light-on-dark styles).
pub(crate) fn max_filter(img: &LumaImage, k: u32) -> LumaImage {
    morph_filter(img, k, MorphPick::Max)
}

/// Extremum direction for the morphological pass.
#[derive(Clone, Copy)]
enum MorphPick {
    Min,
    Max,
}

impl MorphPick {
    /// `true` when `candidate` makes `incumbent` redundant for every FUTURE
    /// window (i.e. candidate is at-least-as-extreme) — the monotonic-deque
    /// eviction test. `>=` / `<=` (not strict) keeps the LAST equal index,
    /// which matches the naive fold's value (ties pick the same u8 anyway).
    #[inline]
    fn dominates(self, candidate: u8, incumbent: u8) -> bool {
        match self {
            Self::Min => candidate <= incumbent,
            Self::Max => candidate >= incumbent,
        }
    }
}

/// 1D clamped-border windowed extremum via a monotonic deque — `out[i]` =
/// extremum over `src[i-r ..= i+r]` clamped to `[0, n-1]`. Amortized O(n):
/// each index enters and leaves `deque` once. Bit-identical to the naive
/// `fold` over the same clamped window. `deque`/`out` are caller-owned
/// scratch (reused across rows/cols), `src` may be strided via `stride`.
fn windowed_extremum_strided(
    src: &[u8],
    n: usize,
    stride: usize,
    r: usize,
    pick: MorphPick,
    deque: &mut Vec<usize>,
    out: &mut [u8],
) {
    deque.clear();
    // `head` points at the front (current extremum index) of `deque`.
    let mut head = 0usize;
    // seed the window for output 0: indices 0 ..= min(r, n-1)
    for j in 0..=r.min(n - 1) {
        let v = src[j * stride];
        while deque.len() > head && pick.dominates(v, src[deque[deque.len() - 1] * stride]) {
            deque.pop();
        }
        deque.push(j);
    }
    for i in 0..n {
        // window for i is [i-r, i+r] clamped; right edge enters first
        let hi = (i + r).min(n - 1);
        // push indices up to `hi` that have not been pushed yet
        // (already seeded up to min(r, n-1); subsequent i grow `hi` by ≤1)
        if i > 0 {
            let prev_hi = (i - 1 + r).min(n - 1);
            for j in (prev_hi + 1)..=hi {
                let v = src[j * stride];
                while deque.len() > head && pick.dominates(v, src[deque[deque.len() - 1] * stride])
                {
                    deque.pop();
                }
                deque.push(j);
            }
        }
        // expire the left edge: drop front indices < i-r
        let lo = i.saturating_sub(r);
        while deque[head] < lo {
            head += 1;
        }
        out[i * stride] = src[deque[head] * stride];
    }
}

fn morph_filter(img: &LumaImage, k: u32, pick: MorphPick) -> LumaImage {
    let r = ((k.max(3) | 1) / 2) as usize; // odd window, radius ≥ 1
    let (w, h) = (img.width() as usize, img.height() as usize);
    let (rx, ry) = (r.min(w.saturating_sub(1)), r.min(h.saturating_sub(1)));
    if rx == 0 && ry == 0 {
        return img.clone();
    }
    let src = img.data();
    let mut deque: Vec<usize> = Vec::with_capacity(2 * r + 2);
    // horizontal pass: out[y][x] = pick over src[y][x-rx ..= x+rx] (clamped)
    let mut horiz = vec![0u8; w * h];
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        let out = &mut horiz[y * w..(y + 1) * w];
        windowed_extremum_strided(row, w, 1, rx, pick, &mut deque, out);
    }
    // vertical pass on the horizontal result — stride = w (column walk).
    let mut data = vec![0u8; w * h];
    for x in 0..w {
        windowed_extremum_strided(&horiz[x..], h, w, ry, pick, &mut deque, &mut data[x..]);
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "w/h come from the source image's u32 dimensions"
    )]
    LumaImage::new(data, w as u32, h as u32)
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
    fn min_filter_grows_dark_pixel_to_exact_kernel_block() {
        // single dark pixel at the center of a 5×5 white field
        let mut data = vec![255u8; 25];
        data[12] = 0;
        let out = min_filter(&luma(&data, 5, 5), 3);
        let mut want = [255u8; 25];
        for y in 1..=3 {
            for x in 1..=3 {
                want[y * 5 + x] = 0;
            }
        }
        assert_eq!(out.data(), &want[..]);
    }

    #[test]
    fn max_filter_is_the_exact_mirror_on_inverted_input() {
        let mut data = vec![0u8; 25];
        data[12] = 255;
        let out = max_filter(&luma(&data, 5, 5), 3);
        let mut want = [0u8; 25];
        for y in 1..=3 {
            for x in 1..=3 {
                want[y * 5 + x] = 255;
            }
        }
        assert_eq!(out.data(), &want[..]);
    }

    #[test]
    fn morph_filter_runs_the_vertical_pass_on_single_column_images() {
        // 1×3 column: rx = 0 but ry = 1 — the vertical pass must still run
        // (the early-return fires only when BOTH radii collapse).
        let out = min_filter(&luma(&[200, 5, 200], 1, 3), 3);
        assert_eq!(out.data(), &[5, 5, 5]);
        // true no-op case: 1×1
        let out = min_filter(&luma(&[42], 1, 1), 9);
        assert_eq!(out.data(), &[42]);
    }

    #[test]
    fn morph_filter_clamps_kernel_to_image_and_bumps_even_k() {
        // k=4 bumps to 5 (radius 2); on a 3×3 image the radius clamps to 2…
        // then to dim-1=2 — a corner dark pixel floods the full image.
        let mut data = vec![255u8; 9];
        data[0] = 7;
        let out = min_filter(&luma(&data, 3, 3), 4);
        assert!(out.data().iter().all(|&p| p == 7), "{:?}", out.data());
        // k below the floor (k=1) behaves as k=3
        let single = min_filter(&luma(&[9, 255], 2, 1), 1);
        assert_eq!(single.data(), &[9, 9]);
    }

    /// Naive O(w·h·k) clamped-window extremum — the REFERENCE the fast
    /// sliding-deque `morph_filter` must reproduce bit-for-bit. This is the
    /// exact algorithm the crate shipped before the deque rewrite.
    fn morph_naive(img: &LumaImage, k: u32, pick: fn(u8, u8) -> u8) -> Vec<u8> {
        let r = ((k.max(3) | 1) / 2) as usize;
        let (w, h) = (img.width() as usize, img.height() as usize);
        let (rx, ry) = (r.min(w.saturating_sub(1)), r.min(h.saturating_sub(1)));
        if rx == 0 && ry == 0 {
            return img.data().to_vec();
        }
        let src = img.data();
        let mut horiz = vec![0u8; w * h];
        for y in 0..h {
            let row = &src[y * w..(y + 1) * w];
            for x in 0..w {
                let lo = x.saturating_sub(rx);
                let hi = (x + rx).min(w - 1);
                horiz[y * w + x] = row[lo..=hi].iter().copied().fold(row[x], pick);
            }
        }
        let mut data = vec![0u8; w * h];
        for y in 0..h {
            let lo = y.saturating_sub(ry);
            let hi = (y + ry).min(h - 1);
            for x in 0..w {
                let mut acc = horiz[lo * w + x];
                for row in (lo + 1)..=hi {
                    acc = pick(acc, horiz[row * w + x]);
                }
                data[y * w + x] = acc;
            }
        }
        data
    }

    /// The deque rewrite is an O(w·h) optimization that MUST be output-
    /// identical to the naive fold for every shape and kernel — a divergence
    /// would silently change which artistic codes the morph rungs recover.
    /// Deterministic LCG covers a spread of dims (incl. 1×N, N×1, even/odd k).
    #[test]
    fn morph_deque_matches_naive_fold_exhaustively() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |bound: u32| -> u32 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as u32 % bound
        };
        for _ in 0..240 {
            let w = 1 + next(40);
            let h = 1 + next(40);
            let k = 1 + next(23); // spans below-floor, even, odd, > dim
            #[expect(clippy::cast_possible_truncation, reason = "next(256) ∈ 0..256")]
            let data: Vec<u8> = (0..w * h).map(|_| next(256) as u8).collect();
            let img = luma(&data, w, h);
            assert_eq!(
                min_filter(&img, k).data(),
                &morph_naive(&img, k, u8::min)[..],
                "MIN mismatch w={w} h={h} k={k}"
            );
            assert_eq!(
                max_filter(&img, k).data(),
                &morph_naive(&img, k, u8::max)[..],
                "MAX mismatch w={w} h={h} k={k}"
            );
        }
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
