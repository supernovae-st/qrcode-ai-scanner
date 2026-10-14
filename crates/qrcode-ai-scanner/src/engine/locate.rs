//! The re-read: locating a read's declarations at the engine's own corner
//! points (07-pipeline property 6 § The re-read).
//!
//! rxing's zxing-cpp port reports the payload bytes and, through its AIM
//! modifier, whether a designator or an FNC1 indicator exists — never where.
//! Its result points are the symbol's outer corners. Sampling the attempt
//! image there, the scanner reads the format information, the version
//! information and the data modules itself, error-corrects them (plain RS)
//! and parses the stream with its own parser. A parse that reproduces the
//! payload byte for byte and agrees with the modifier locates the
//! declarations and observes the Kanji/Hanzi segments; a malformed stream
//! whose tolerant parse reproduces the payload refuses the read; anything
//! else leaves the engine's evidence. The re-read never changes a read's
//! bytes and keeps its geometry internal.
//!
//! The re-read also measures the correction margin of the word it reads
//! there (07-pipeline § Acceptance). A port read with nothing to locate is
//! re-read for that margin alone, at EC level L or M and on versions 1 and 2
//! only ([`MARGIN_VERSIONS`]), and nothing else of it changes.
//!
//! Bounded and deterministic: versions 1-40 (an rqrr read: its measured
//! version; a margin re-read: versions 1 and 2) × {as read, transposed} ×
//! three sampler passes — at most [`MAX_PROBES`] format probes and as many
//! error-correction attempts per call (12 for a margin re-read); no clock,
//! no RNG, no budget. Score cells and probes never run it.

use std::ops::RangeInclusive;

use crate::engine::content::{DoubleByteSpan, EciMark, Fnc1};
use crate::input::LumaImage;
use crate::matrix::sampler::{Homography, sample_bilinear};
use crate::matrix::zigzag::zigzag_positions;
use crate::report::{EcLevel, Point};
use crate::rescue::bitstream::PortRead;
use crate::rescue::{Acceptance, Rederived};

/// Format probes — and error-correction attempts — one call may spend:
/// 40 versions × 2 orientations × 3 sampler passes.
pub(crate) const MAX_PROBES: usize = 240;

/// The versions a margin re-read tries ([`LocateHint::for_margin`]): errors
/// alone pass the ISO/IEC 18004 Annex B reserve only on version 1 at EC
/// levels L and M and on version 2 at L.
#[cfg_attr(
    not(feature = "engine-rxing"),
    allow(
        dead_code,
        reason = "only rxing's port reads are re-read for their margin"
    )
)]
pub(crate) const MARGIN_VERSIONS: RangeInclusive<u8> = 1..=2;

/// How a read's corner points frame the module grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(feature = "engine-rxing"),
    allow(
        dead_code,
        reason = "only rxing's reads frame the grid by its outer corners"
    )
)]
pub(crate) enum Frame {
    /// rxing's result points: the symbol's outer corners plus half a pixel,
    /// clockwise from the top-left finder.
    OuterCorners,
    /// rqrr's grid bounds, which span `N + 1` module cells (quirc heritage,
    /// `matrix::sampler`).
    GridBounds,
}

/// What the engine stated about a read the scanner re-reads.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LocateHint {
    /// The corner points, in the attempt image's pixels.
    pub(crate) points: [Point; 4],
    /// How they frame the grid.
    pub(crate) frame: Frame,
    /// The engine read the symbol photometrically inverted (rxing's
    /// `IS_INVERTED`).
    pub(crate) inverted: bool,
    /// The error-correction level the engine read.
    pub(crate) ec: EcLevel,
    /// The versions to try: the one rqrr measured, every version for a port
    /// read's declarations, [`MARGIN_VERSIONS`] for its margin alone.
    pub(crate) versions: RangeInclusive<u8>,
    /// The AIM modifier digit (`]Qn`) the engine stated; `None` for rqrr,
    /// which decodes neither FNC1 mode and states nothing of ECI.
    pub(crate) modifier: Option<u8>,
    /// The re-read locates the read's declarations and segments; otherwise
    /// it measures its word's margin and changes nothing else of the read.
    pub(crate) declarations: bool,
}

impl LocateHint {
    /// The re-read that measures a port read's word margin and nothing
    /// else: at EC level L or M, on [`MARGIN_VERSIONS`]. `None` at Q and H,
    /// whose reserve errors alone never pass, and for rqrr's reads, which
    /// carry their own word's margin.
    #[cfg_attr(
        not(feature = "engine-rxing"),
        allow(
            dead_code,
            reason = "only rxing's port reads are re-read for their margin"
        )
    )]
    pub(crate) fn for_margin(&self) -> Option<Self> {
        let measurable = matches!(self.ec, EcLevel::L | EcLevel::M);
        (self.frame == Frame::OuterCorners && measurable).then(|| Self {
            versions: MARGIN_VERSIONS,
            declarations: false,
            ..self.clone()
        })
    }
}

/// What the re-read established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Located {
    /// The stream at the read's corners reproduces its payload: its
    /// declarations, FNC1 position and double-byte segments.
    Parsed {
        /// The designators, in stream order (empty: none).
        eci: Vec<EciMark>,
        /// The FNC1 position the stream declares.
        fnc1: Fnc1,
        /// The Kanji/Hanzi segments the parse observed.
        segments: Vec<DoubleByteSpan>,
        /// The version that verified.
        version: u8,
        /// The mask the format information gave.
        mask: u8,
        /// The grid verified transposed (a mirrored symbol).
        mirrored: bool,
        /// The worst block's correction margin of the word read there
        /// (`rescue::margin`).
        margin: i32,
    },
    /// A port read's own grid is a stream ISO/IEC 18004 does not define, and
    /// a parse with the port's tolerances reproduces its payload: what that
    /// reader reports for it.
    Refused {
        /// The port's reading of the refused stream.
        port: PortRead,
    },
    /// Nothing verified the read.
    Unverified,
}

/// What one call spent — pinned by tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Spent {
    /// Format-information reads.
    pub(crate) format_probes: usize,
    /// Error-correction attempts (a format word, and from version 7 the
    /// version word, read right).
    pub(crate) rs_attempts: usize,
}

/// Re-read `payload` at the read's corners in `img` (the attempt image).
pub(crate) fn locate(img: &LumaImage, hint: &LocateHint, payload: &[u8]) -> Located {
    locate_spending(img, hint, payload).0
}

/// [`locate`], with what it spent.
pub(crate) fn locate_spending(
    img: &LumaImage,
    hint: &LocateHint,
    payload: &[u8],
) -> (Located, Spent) {
    let mut spent = Spent::default();
    let black = Binarized::of(img);
    let versions: Vec<u8> = hint
        .versions
        .clone()
        .filter(|version| (1..=40).contains(version))
        .collect();
    let passes = [
        Pass::Binarized {
            dark_is_black: !hint.inverted,
        },
        Pass::Binarized {
            dark_is_black: hint.inverted,
        },
        Pass::Luma {
            dark_is_low: !hint.inverted,
        },
    ];
    for pass in passes {
        for mirrored in [false, true] {
            for &version in &versions {
                let Some(grid) = Grid::new(img, &black, hint, version, pass, mirrored) else {
                    continue;
                };
                if spent.format_probes >= MAX_PROBES {
                    return (Located::Unverified, spent);
                }
                spent.format_probes += 1;
                let Some(mask) = grid.format_mask(hint.ec) else {
                    continue;
                };
                if version >= 7 && grid.version_word() != Some(version) {
                    continue;
                }
                spent.rs_attempts += 1;
                let stream = grid.masked_stream();
                match crate::rescue::rederive(&stream, version, hint.ec, mask, Acceptance::Engine) {
                    Rederived::Parsed { parsed, margin }
                        if parsed.bytes == payload
                            && agrees(!parsed.eci.is_empty(), parsed.fnc1, hint.modifier) =>
                    {
                        let located = Located::Parsed {
                            eci: parsed.eci,
                            fnc1: parsed.fnc1,
                            segments: parsed.segments,
                            version,
                            mask,
                            mirrored,
                            margin,
                        };
                        return (located, spent);
                    }
                    // only a port read is refused: its reader tolerates what
                    // the parse refuses, rqrr's never reaches a parse
                    Rederived::Malformed { port: Some(port) }
                        if hint.modifier.is_some()
                            && port.bytes == payload
                            && agrees(port.signalled, port.fnc1, hint.modifier) =>
                    {
                        return (Located::Refused { port }, spent);
                    }
                    _ => {}
                }
            }
        }
    }
    (Located::Unverified, spent)
}

/// Whether a parse's ECI presence and FNC1 position are what the engine's
/// modifier stated (ISO/IEC 18004 Annex F: 2, 4, 6 = ECI; 3, 4 = FNC1 first;
/// 5, 6 = FNC1 second). Without a modifier (rqrr) only FNC1 none agrees.
fn agrees(eci: bool, fnc1: Fnc1, modifier: Option<u8>) -> bool {
    let Some(modifier) = modifier else {
        return fnc1 == Fnc1::None;
    };
    let position = match modifier {
        3 | 4 => Fnc1::First,
        5 | 6 => Fnc1::Second,
        _ => Fnc1::None,
    };
    eci == matches!(modifier, 2 | 4 | 6) && fnc1 == position
}

/// One sampler pass.
#[derive(Debug, Clone, Copy)]
enum Pass {
    /// The engine's binarized image (rxing's hybrid binarizer).
    Binarized {
        /// A dark module is a black pixel (else a white one).
        dark_is_black: bool,
    },
    /// The luma itself, thresholded at the midpoint between the 2nd and
    /// 98th percentile of the grid's module samples.
    Luma {
        /// A dark module is below the threshold (else above).
        dark_is_low: bool,
    },
}

/// The attempt image binarized as rxing binarizes it — computed at most once
/// per call, and only when a pass needs it.
struct Binarized<'a> {
    img: &'a LumaImage,
    #[cfg(feature = "engine-rxing")]
    matrix: std::cell::OnceCell<Option<rxing::common::BitMatrix>>,
}

impl<'a> Binarized<'a> {
    fn of(img: &'a LumaImage) -> Self {
        Self {
            img,
            #[cfg(feature = "engine-rxing")]
            matrix: std::cell::OnceCell::new(),
        }
    }

    /// Whether pixel `(x, y)` binarizes black; `None` without a binarized
    /// image (no rxing, or it failed).
    #[cfg(feature = "engine-rxing")]
    fn black(&self, x: u32, y: u32) -> Option<bool> {
        use rxing::Binarizer as _;
        let matrix = self.matrix.get_or_init(|| {
            let source = rxing::Luma8LuminanceSource::new(
                self.img.data().to_vec(),
                self.img.width(),
                self.img.height(),
            );
            rxing::common::HybridBinarizer::new(source)
                .get_black_matrix()
                .ok()
                .cloned()
        });
        matrix
            .as_ref()
            .map(|m| x < m.width() && y < m.height() && m.get(x, y))
    }

    #[cfg(not(feature = "engine-rxing"))]
    fn black(&self, _x: u32, _y: u32) -> Option<bool> {
        let _ = self.img;
        None
    }
}

/// One candidate grid: a version, an orientation and a sampler pass over
/// the read's corners.
struct Grid<'a> {
    img: &'a LumaImage,
    black: &'a Binarized<'a>,
    to_image: Homography,
    /// Module count per side.
    size: usize,
    /// The unit-square denominator: `size` (outer corners) or `size + 1`.
    cells: f32,
    pass: Pass,
    mirrored: bool,
    /// The luma pass's threshold.
    threshold: f32,
}

impl<'a> Grid<'a> {
    /// `None` when the version's module pitch would be below one pixel, the
    /// corners do not span a quad, or a pass has no image.
    #[expect(
        clippy::cast_precision_loss,
        reason = "module counts ≤ 177 are exact in f32"
    )]
    fn new(
        img: &'a LumaImage,
        black: &'a Binarized<'a>,
        hint: &LocateHint,
        version: u8,
        pass: Pass,
        mirrored: bool,
    ) -> Option<Self> {
        let size = usize::from(version) * 4 + 17;
        let (quad, cells) = match hint.frame {
            // outer corners + 0.5 px → the module boundary in the sampler's
            // pixel-centre coordinates (pixel i at i)
            Frame::OuterCorners => (hint.points.map(|p| (p.x - 1.0, p.y - 1.0)), size as f32),
            Frame::GridBounds => (hint.points.map(|p| (p.x, p.y)), size as f32 + 1.0),
        };
        let span = side_lengths(&quad) / 4.0;
        if span / cells < 1.0 {
            return None;
        }
        let to_image = Homography::unit_square_to_quad(quad)?;
        let mut grid = Self {
            img,
            black,
            to_image,
            size,
            cells,
            pass,
            mirrored,
            threshold: 0.0,
        };
        match pass {
            Pass::Binarized { .. } => {
                grid.black.black(0, 0)?;
            }
            Pass::Luma { .. } => grid.threshold = grid.midpoint()?,
        }
        Some(grid)
    }

    /// The image point at the centre of module `(column, row)` of the
    /// symbol as read.
    #[expect(
        clippy::cast_precision_loss,
        reason = "module indices ≤ 177 are exact in f32"
    )]
    fn centre(&self, column: usize, row: usize) -> Option<(f32, f32)> {
        let (u, v) = (
            (column as f32 + 0.5) / self.cells,
            (row as f32 + 0.5) / self.cells,
        );
        self.to_image.apply(u, v)
    }

    /// Whether module `(row, column)` of the symbol is dark — transposed for
    /// a mirrored reading.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded pixel coordinates, range-checked before the cast"
    )]
    fn dark(&self, row: usize, column: usize) -> bool {
        let (column, row) = if self.mirrored {
            (row, column)
        } else {
            (column, row)
        };
        let Some((x, y)) = self.centre(column, row) else {
            return false;
        };
        match self.pass {
            Pass::Binarized { dark_is_black } => {
                let (x, y) = (x.round(), y.round());
                if x < 0.0 || y < 0.0 {
                    return !dark_is_black;
                }
                let black = self.black.black(x as u32, y as u32).unwrap_or(false);
                black == dark_is_black
            }
            Pass::Luma { dark_is_low } => {
                let luma = f32::from(sample_bilinear(self.img, x, y));
                (luma < self.threshold) == dark_is_low
            }
        }
    }

    /// The midpoint between the 2nd and 98th percentile of the grid's module
    /// samples (at most a 64 × 64 lattice of modules). `None` for a flat
    /// grid.
    fn midpoint(&self) -> Option<f32> {
        let step = self.size.div_ceil(64).max(1);
        let mut samples: Vec<u8> = (0..self.size)
            .step_by(step)
            .flat_map(|row| {
                (0..self.size)
                    .step_by(step)
                    .map(move |column| (row, column))
            })
            .filter_map(|(row, column)| self.centre(column, row))
            .map(|(x, y)| sample_bilinear(self.img, x, y))
            .collect();
        if samples.len() < 16 {
            return None;
        }
        samples.sort_unstable();
        let lo = f32::from(samples[samples.len() / 50]);
        let hi = f32::from(samples[samples.len() - 1 - samples.len() / 50]);
        (hi - lo >= 16.0).then(|| f32::midpoint(lo, hi))
    }

    /// The mask of the format word that decodes, within three bit errors,
    /// to the read's EC level — either copy.
    fn format_mask(&self, ec: EcLevel) -> Option<u8> {
        self.format_words()
            .into_iter()
            .filter_map(decode_format)
            .find(|&(level, _)| level == ec)
            .map(|(_, mask)| mask)
    }

    /// The two format-information copies, unmasked (`^ 0x5412`), in rqrr's
    /// bit order.
    fn format_words(&self) -> [u16; 2] {
        const XS: [usize; 15] = [8, 8, 8, 8, 8, 8, 8, 8, 7, 5, 4, 3, 2, 1, 0];
        const YS: [usize; 15] = [0, 1, 2, 3, 4, 5, 7, 8, 8, 8, 8, 8, 8, 8, 8];
        let size = self.size;
        let mut first = 0u16;
        for i in (0..15).rev() {
            first = (first << 1) | u16::from(self.dark(YS[i], XS[i]));
        }
        let mut second = 0u16;
        for i in 0..7 {
            second = (second << 1) | u16::from(self.dark(size - 1 - i, 8));
        }
        for i in 0..8 {
            second = (second << 1) | u16::from(self.dark(8, size - 8 + i));
        }
        [first ^ 0x5412, second ^ 0x5412]
    }

    /// The version the version information states, within three bit
    /// errors — either copy.
    fn version_word(&self) -> Option<u8> {
        let size = self.size;
        let mut top_right = 0u32;
        let mut bottom_left = 0u32;
        for bit in 0..18 {
            if self.dark(bit / 3, size - 11 + bit % 3) {
                top_right |= 1 << bit;
            }
            if self.dark(size - 11 + bit % 3, bit / 3) {
                bottom_left |= 1 << bit;
            }
        }
        decode_version(top_right).or_else(|| decode_version(bottom_left))
    }

    /// The data modules in placement order, still masked — the stream rqrr
    /// would read from this grid.
    fn masked_stream(&self) -> crate::engine::MaskedStream {
        let positions = zigzag_positions((self.size - 17) / 4);
        let mut bits = vec![0u8; positions.len().div_ceil(8)];
        for (k, &(row, column)) in positions.iter().enumerate() {
            if self.dark(row, column) {
                bits[k / 8] |= 1 << (7 - k % 8);
            }
        }
        crate::engine::MaskedStream {
            bits,
            bit_len: positions.len(),
        }
    }
}

/// The perimeter of a quad.
fn side_lengths(quad: &[(f32, f32); 4]) -> f32 {
    (0..4)
        .map(|i| {
            let (a, b) = (quad[i], quad[(i + 1) % 4]);
            (b.0 - a.0).hypot(b.1 - a.1)
        })
        .sum()
}

/// The remainder of `value` (degree < 31) divided by the BCH generator.
fn bch_remainder(mut value: u32, generator: u32) -> u32 {
    let Some(degree) = generator.checked_ilog2() else {
        return value;
    };
    while let Some(top) = value.checked_ilog2()
        && top >= degree
    {
        value ^= generator << (top - degree);
    }
    value
}

/// The format word within three bit errors of an unmasked read: its EC
/// level (format bits M=0 · L=1 · H=2 · Q=3) and mask.
fn decode_format(word: u16) -> Option<(EcLevel, u8)> {
    (0u32..32)
        .find(|&data| {
            let code = (data << 10) | bch_remainder(data << 10, 0x537);
            (code ^ u32::from(word)).count_ones() <= 3
        })
        .and_then(|data| {
            let level = match data >> 3 {
                0 => EcLevel::M,
                1 => EcLevel::L,
                2 => EcLevel::H,
                _ => EcLevel::Q,
            };
            u8::try_from(data & 7).ok().map(|mask| (level, mask))
        })
}

/// The version (7-40) whose version word is within three bit errors.
fn decode_version(word: u32) -> Option<u8> {
    (7u32..=40)
        .find(|&version| {
            let code = (version << 12) | bch_remainder(version << 12, 0x1F25);
            (code ^ word).count_ones() <= 3
        })
        .and_then(|version| u8::try_from(version).ok())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_precision_loss,
        clippy::float_cmp
    )]

    use qrcode::bits::Bits;
    use qrcode::canvas::{Canvas, MaskPattern};

    use super::*;
    use crate::engine::content::DoubleByteMode;

    const SCALE: usize = 4;
    const QUIET: usize = 4;
    const MASKS: [MaskPattern; 8] = [
        MaskPattern::Checkerboard,
        MaskPattern::HorizontalLines,
        MaskPattern::VerticalLines,
        MaskPattern::DiagonalLines,
        MaskPattern::LargeCheckerboard,
        MaskPattern::Fields,
        MaskPattern::Diamonds,
        MaskPattern::Meadow,
    ];

    type Push = fn(&mut Bits) -> qrcode::types::QrResult<()>;

    /// A symbol built from `push` with mask `mask`, rendered at 4 px per
    /// module with a 4-module quiet zone (transposed on request), and the
    /// points rxing reports for it: the outer corners plus half a pixel.
    /// `None` when the data does not fit the version.
    fn render(
        version: i16,
        ec: qrcode::EcLevel,
        mask: usize,
        transposed: bool,
        push: Push,
    ) -> Option<(LumaImage, [Point; 4])> {
        let (colors, width) = symbol_colors(version, ec, mask, push)?;
        raster(&colors, width, transposed)
    }

    /// The module colours of a symbol built from `push` with mask `mask`,
    /// row by row, and its width.
    fn symbol_colors(
        version: i16,
        ec: qrcode::EcLevel,
        mask: usize,
        push: Push,
    ) -> Option<(Vec<qrcode::Color>, usize)> {
        let symbol = qrcode::Version::Normal(version);
        let mut bits = Bits::new(symbol);
        push(&mut bits).ok()?;
        bits.push_terminator(ec).ok()?;
        let (data, ecc) = qrcode::ec::construct_codewords(&bits.into_bytes(), symbol, ec).ok()?;
        let mut canvas = Canvas::new(symbol, ec);
        canvas.draw_all_functional_patterns();
        canvas.draw_data(&data, &ecc);
        canvas.apply_mask(MASKS[mask]);
        let width = usize::try_from(symbol.width()).ok()?;
        Some((canvas.into_colors(), width))
    }

    /// [`render`]'s raster of module colours.
    fn raster(
        colors: &[qrcode::Color],
        width: usize,
        transposed: bool,
    ) -> Option<(LumaImage, [Point; 4])> {
        let side = (width + 2 * QUIET) * SCALE;
        let mut pixels = vec![255u8; side * side];
        for row in 0..width {
            for column in 0..width {
                let (module_row, module_column) = if transposed {
                    (column, row)
                } else {
                    (row, column)
                };
                if colors[module_row * width + module_column] == qrcode::Color::Dark {
                    for dy in 0..SCALE {
                        for dx in 0..SCALE {
                            let pixel_row = (row + QUIET) * SCALE + dy;
                            let pixel_column = (column + QUIET) * SCALE + dx;
                            pixels[pixel_row * side + pixel_column] = 0;
                        }
                    }
                }
            }
        }
        let lo = (QUIET * SCALE) as f32 + 0.5;
        let hi = ((QUIET + width) * SCALE) as f32 + 0.5;
        let points = [
            Point { x: lo, y: lo },
            Point { x: hi, y: lo },
            Point { x: hi, y: hi },
            Point { x: lo, y: hi },
        ];
        let side = u32::try_from(side).ok()?;
        Some((LumaImage::new(pixels, side, side), points))
    }

    fn level(ec: qrcode::EcLevel) -> EcLevel {
        match ec {
            qrcode::EcLevel::L => EcLevel::L,
            qrcode::EcLevel::M => EcLevel::M,
            qrcode::EcLevel::Q => EcLevel::Q,
            qrcode::EcLevel::H => EcLevel::H,
        }
    }

    fn hint(points: [Point; 4], ec: EcLevel, modifier: u8) -> LocateHint {
        LocateHint {
            points,
            frame: Frame::OuterCorners,
            inverted: false,
            ec,
            versions: 1..=40,
            modifier: Some(modifier),
            declarations: true,
        }
    }

    fn marks(list: &[(u32, usize)]) -> Vec<EciMark> {
        list.iter()
            .map(|&(value, start)| EciMark { value, start })
            .collect()
    }

    /// (designators, payload, push)
    type Case = (&'static [(u32, usize)], &'static [u8], Push);

    const CASES: [Case; 5] = [
        (&[], b"abc", |b| b.push_byte_data(b"abc")),
        (&[(4, 0)], &[0xA1], |b| {
            b.push_eci_designator(4)?;
            b.push_byte_data(&[0xA1])
        }),
        (&[(26, 0)], &[0xC3, 0xA9], |b| {
            b.push_eci_designator(26)?;
            b.push_byte_data(&[0xC3, 0xA9])
        }),
        (&[(4, 0), (26, 2)], &[0xA1, 0xA2, 0x41], |b| {
            b.push_eci_designator(4)?;
            b.push_byte_data(&[0xA1, 0xA2])?;
            b.push_eci_designator(26)?;
            b.push_byte_data(&[0x41])
        }),
        (&[(7, 5)], b"12345\xBF", |b| {
            b.push_numeric_data(b"12345")?;
            b.push_eci_designator(7)?;
            b.push_byte_data(&[0xBF])
        }),
    ];

    #[test]
    fn locate_replays_clean_symbols() {
        // every version, EC level and designator list the data fits, each
        // with a mask of its own, as read and transposed
        let mut located = 0;
        for (i, version) in [1i16, 2, 3, 5, 6, 7, 10, 14, 25, 40]
            .into_iter()
            .enumerate()
        {
            for (j, ec) in [
                qrcode::EcLevel::L,
                qrcode::EcLevel::M,
                qrcode::EcLevel::Q,
                qrcode::EcLevel::H,
            ]
            .into_iter()
            .enumerate()
            {
                for (k, (list, payload, push)) in CASES.into_iter().enumerate() {
                    let mask = (i + j + k) % 8;
                    for transposed in [false, true] {
                        let Some((img, points)) = render(version, ec, mask, transposed, push)
                        else {
                            continue;
                        };
                        let modifier = if list.is_empty() { 1 } else { 2 };
                        let (found, spent) =
                            locate_spending(&img, &hint(points, level(ec), modifier), payload);
                        let case = format!("v{version} {ec:?} {list:?} mask {mask} T{transposed}");
                        // a clean word: no error spent, whatever its blocks keep
                        let margin = match found {
                            Located::Parsed { margin, .. } if margin >= 0 => margin,
                            _ => panic!("{case}: {found:?}"),
                        };
                        let expected = Located::Parsed {
                            eci: marks(list),
                            fnc1: Fnc1::None,
                            segments: Vec::new(),
                            version: u8::try_from(version).unwrap(),
                            mask: u8::try_from(mask).unwrap(),
                            mirrored: transposed,
                            margin,
                        };
                        assert_eq!(found, expected, "{case}");
                        assert!(spent.format_probes <= MAX_PROBES, "{case}: {spent:?}");
                        located += 1;
                    }
                }
            }
        }
        assert!(located >= 300, "{located} symbols replayed");
    }

    #[test]
    fn located_segments_are_the_parse_s() {
        // Kanji after byte data: the re-read observes the segment
        let push: Push = |b| {
            b.push_byte_data(b"QR")?;
            b.push_kanji_data(&[0x93, 0xFA])
        };
        let (img, points) = render(2, qrcode::EcLevel::M, 3, false, push).unwrap();
        let found = locate(&img, &hint(points, EcLevel::M, 1), b"QR\x93\xFA");
        let Located::Parsed { segments, eci, .. } = found else {
            panic!("{found:?}");
        };
        assert!(eci.is_empty());
        assert_eq!(
            segments,
            [DoubleByteSpan {
                mode: DoubleByteMode::Kanji,
                start: 2,
                end: 4
            }]
        );
    }

    #[test]
    fn a_read_elsewhere_or_of_other_bytes_is_not_located() {
        // the points shifted by two modules, one payload byte changed,
        // another EC level, a modifier the stream disagrees with
        let (img, points) = render(3, qrcode::EcLevel::M, 5, false, CASES[1].2).unwrap();
        let shift = (2 * SCALE) as f32;
        let shifted = points.map(|p| Point {
            x: p.x + shift,
            y: p.y + shift,
        });
        assert_eq!(
            locate(&img, &hint(shifted, EcLevel::M, 2), &[0xA1]),
            Located::Unverified
        );
        assert_eq!(
            locate(&img, &hint(points, EcLevel::M, 2), &[0xA2]),
            Located::Unverified
        );
        assert_eq!(
            locate(&img, &hint(points, EcLevel::Q, 2), &[0xA1]),
            Located::Unverified
        );
        for modifier in [1, 3, 4] {
            assert_eq!(
                locate(&img, &hint(points, EcLevel::M, modifier), &[0xA1]),
                Located::Unverified,
                "]Q{modifier}"
            );
        }
        // the same call, again: deterministic
        assert!(matches!(
            locate(&img, &hint(points, EcLevel::M, 2), &[0xA1]),
            Located::Parsed { .. }
        ));
    }

    #[test]
    fn a_call_spends_at_most_its_bound() {
        // a textured image where nothing verifies: every version,
        // orientation and pass is probed once — 240, never more
        let side = 400u32;
        let pixels = (0..side * side)
            .map(|i| {
                if (i % side / 3 + i / side / 3).is_multiple_of(2) {
                    0
                } else {
                    255
                }
            })
            .collect();
        let img = LumaImage::new(pixels, side, side);
        let points = [
            Point { x: 20.5, y: 20.5 },
            Point { x: 380.5, y: 20.5 },
            Point { x: 380.5, y: 380.5 },
            Point { x: 20.5, y: 380.5 },
        ];
        let (found, spent) = locate_spending(&img, &hint(points, EcLevel::M, 2), b"abc");
        assert_eq!(found, Located::Unverified);
        assert!(spent.format_probes <= MAX_PROBES, "{spent:?}");
        assert!(spent.rs_attempts <= spent.format_probes, "{spent:?}");
        // an rqrr hint (its measured version) probes that version only
        let rqrr = LocateHint {
            versions: 3..=3,
            modifier: None,
            frame: Frame::GridBounds,
            ..hint(points, EcLevel::M, 2)
        };
        let (_, spent) = locate_spending(&img, &rqrr, b"abc");
        assert!(spent.format_probes <= 6, "{spent:?}");
    }

    #[test]
    fn format_and_version_words_correct_three_bit_errors() {
        for data in 0u32..32 {
            let code = (data << 10) | bch_remainder(data << 10, 0x537);
            let expected = decode_format(u16::try_from(code).unwrap()).unwrap();
            assert_eq!(u32::from(expected.1), data & 7);
            for errors in [0b1u32, 0b101, 0b1_0000_0001_0001] {
                let word = u16::try_from(code ^ (errors << (data % 3))).unwrap();
                assert_eq!(
                    decode_format(word),
                    Some(expected),
                    "{data:05b} ^ {errors:b}"
                );
            }
        }
        for version in 7u32..=40 {
            let code = (version << 12) | bch_remainder(version << 12, 0x1F25);
            for errors in [0u32, 0b1, 0b1000_0000_0001, 0b1_0000_0001_0000_0001] {
                assert_eq!(
                    decode_version(code ^ errors),
                    u8::try_from(version).ok(),
                    "v{version} ^ {errors:b}"
                );
            }
        }
        assert_eq!(bch_remainder(0b101, 0), 0b101, "no generator, no division");
    }

    #[test]
    fn a_margin_re_read_measures_versions_1_and_2_only() {
        // a port read at L or M of a v2 symbol: located on its own word, with
        // that word's margin (v2-L: ⌊(10 − 2)/2⌋ − 0 = 4); twelve probes at
        // most, the same every time
        let plain: Push = |b| b.push_byte_data(b"https://qrc-ai.com/v2l");
        let (img, points) = render(2, qrcode::EcLevel::L, 1, false, plain).unwrap();
        let margin_hint = hint(points, EcLevel::L, 1)
            .for_margin()
            .expect("L: a margin to measure");
        assert_eq!(margin_hint.versions, MARGIN_VERSIONS);
        assert!(!margin_hint.declarations);
        let (found, spent) = locate_spending(&img, &margin_hint, b"https://qrc-ai.com/v2l");
        let Located::Parsed {
            margin, version, ..
        } = found
        else {
            panic!("{found:?}");
        };
        assert_eq!((version, margin), (2, 4));
        assert!(
            spent.format_probes <= 12 && spent.rs_attempts <= 12,
            "{spent:?}"
        );
        assert_eq!(
            locate_spending(&img, &margin_hint, b"https://qrc-ai.com/v2l"),
            (found, spent)
        );
        // a v5 symbol is no version the margin re-read tries: unverified
        let (img, points) = render(5, qrcode::EcLevel::M, 2, false, plain).unwrap();
        let margin_hint = hint(points, EcLevel::M, 1).for_margin().unwrap();
        let (found, spent) = locate_spending(&img, &margin_hint, b"https://qrc-ai.com/v2l");
        assert_eq!(found, Located::Unverified);
        assert!(spent.format_probes <= 12, "{spent:?}");
        // Q and H reads: errors alone never pass their reserve
        for ec in [EcLevel::Q, EcLevel::H] {
            assert!(hint(points, ec, 1).for_margin().is_none(), "{ec:?}");
        }
        // rqrr's reads carry their own word's margin: never re-read for it
        let rqrr = LocateHint {
            frame: Frame::GridBounds,
            ..hint(points, EcLevel::L, 1)
        };
        assert!(rqrr.for_margin().is_none());
    }

    #[test]
    fn a_located_re_read_reports_its_words_margin() {
        // the declarations re-read too: ECI 4 at v3-M, no error, 26 parity
        // codewords — a margin of 13
        let (img, points) = render(3, qrcode::EcLevel::M, 5, false, CASES[1].2).unwrap();
        let found = locate(&img, &hint(points, EcLevel::M, 2), &[0xA1]);
        assert!(
            matches!(found, Located::Parsed { margin: 13, .. }),
            "{found:?}"
        );
    }

    #[test]
    fn the_modifier_states_the_eci_signal_and_the_fnc1_position() {
        for (modifier, eci, fnc1) in [
            (0, false, Fnc1::None),
            (1, false, Fnc1::None),
            (2, true, Fnc1::None),
            (3, false, Fnc1::First),
            (4, true, Fnc1::First),
            (5, false, Fnc1::Second),
            (6, true, Fnc1::Second),
        ] {
            assert!(agrees(eci, fnc1, Some(modifier)), "]Q{modifier}");
            assert!(!agrees(!eci, fnc1, Some(modifier)), "]Q{modifier}");
            assert!(
                !agrees(eci, Fnc1::Unobserved, Some(modifier)),
                "]Q{modifier}"
            );
        }
        // rqrr states nothing: only FNC1 none, whatever the declarations
        assert!(agrees(true, Fnc1::None, None) && agrees(false, Fnc1::None, None));
        assert!(!agrees(false, Fnc1::First, None));
    }

    /// ISO/IEC 18004 Table C.1: the format information as it lies in the
    /// symbol (XOR 101010000010010), by its five data bits — the EC level
    /// indicator (M 00 · L 01 · H 10 · Q 11), then the mask pattern
    /// reference.
    const FORMAT_INFORMATION: [u16; 32] = [
        0x5412, 0x5125, 0x5E7C, 0x5B4B, 0x45F9, 0x40CE, 0x4F97, 0x4AA0, // M
        0x77C4, 0x72F3, 0x7DAA, 0x789D, 0x662F, 0x6318, 0x6C41, 0x6976, // L
        0x1689, 0x13BE, 0x1CE7, 0x19D0, 0x0762, 0x0255, 0x0D0C, 0x083B, // H
        0x355F, 0x3068, 0x3F31, 0x3A06, 0x24B4, 0x2183, 0x2EDA, 0x2BED, // Q
    ];

    /// ISO/IEC 18004 Table D.1: the version information of versions 7-40.
    const VERSION_INFORMATION: [u32; 34] = [
        0x07C94, 0x085BC, 0x09A99, 0x0A4D3, 0x0BBF6, 0x0C762, 0x0D847, 0x0E60D, 0x0F928, 0x10B78,
        0x1145D, 0x12A17, 0x13532, 0x149A6, 0x15683, 0x168C9, 0x177EC, 0x18EC4, 0x191E1, 0x1AFAB,
        0x1B08E, 0x1CC1A, 0x1D33F, 0x1ED75, 0x1F250, 0x209D5, 0x216F0, 0x228BA, 0x2379F, 0x24B0B,
        0x2542E, 0x26A64, 0x27541, 0x28C69,
    ];

    /// The EC level each value of the format information's indicator bits
    /// states, as the encoder and the report name it.
    const INDICATED: [(qrcode::EcLevel, EcLevel); 4] = [
        (qrcode::EcLevel::M, EcLevel::M),
        (qrcode::EcLevel::L, EcLevel::L),
        (qrcode::EcLevel::H, EcLevel::H),
        (qrcode::EcLevel::Q, EcLevel::Q),
    ];

    /// Every error pattern of `weight` bits within `width` bits.
    fn errors(width: u32, weight: u32) -> Vec<u32> {
        (0..1u32 << width)
            .filter(|e| e.count_ones() == weight)
            .collect()
    }

    #[test]
    fn format_information_decodes_within_three_errors_and_never_past() {
        let correctable: Vec<u32> = (0..=3).flat_map(|weight| errors(15, weight)).collect();
        let past = errors(15, 4);
        for (data, &masked) in (0u32..).zip(&FORMAT_INFORMATION) {
            let level = INDICATED[usize::try_from(data >> 3).unwrap()].1;
            let expected = Some((level, u8::try_from(data & 7).unwrap()));
            let word = masked ^ 0x5412;
            assert_eq!(
                (data << 10) | bch_remainder(data << 10, 0x537),
                u32::from(word),
                "{data:05b}: the generator's remainder completes the codeword"
            );
            for &e in &correctable {
                let received = word ^ u16::try_from(e).unwrap();
                assert_eq!(decode_format(received), expected, "{data:05b} ^ {e:015b}");
            }
            // minimum distance 7: four errors never decode to this codeword —
            // to none, or to another one three bits away
            for &e in &past {
                let received = word ^ u16::try_from(e).unwrap();
                assert_ne!(decode_format(received), expected, "{data:05b} ^ {e:015b}");
            }
        }
    }

    #[test]
    fn version_information_decodes_within_three_errors_and_never_past() {
        let correctable: Vec<u32> = (0..=3).flat_map(|weight| errors(18, weight)).collect();
        let past = errors(18, 4);
        for (version, &word) in (7u32..).zip(&VERSION_INFORMATION) {
            assert_eq!(
                (version << 12) | bch_remainder(version << 12, 0x1F25),
                word,
                "v{version}: the generator's remainder completes the codeword"
            );
            let expected = u8::try_from(version).ok();
            for &e in &correctable {
                assert_eq!(decode_version(word ^ e), expected, "v{version} ^ {e:018b}");
            }
            // minimum distance 8: four errors leave every codeword at least
            // four bits away — no version
            for &e in &past {
                assert_eq!(decode_version(word ^ e), None, "v{version} ^ {e:018b}");
            }
        }
    }

    #[test]
    fn each_format_copy_reads_its_table_word_on_its_own() {
        // version 1 at every EC level and mask, as read and transposed: the
        // copy around the top-left finder and the one split between the
        // other two finders each read the Table C.1 word, in both passes
        for (data, &masked) in FORMAT_INFORMATION.iter().enumerate() {
            let (ec, level) = INDICATED[data >> 3];
            let mask = data & 7;
            for transposed in [false, true] {
                let (img, points) = render(1, ec, mask, transposed, CASES[0].2).unwrap();
                let black = Binarized::of(&img);
                let stated = hint(points, level, 1);
                for pass in [
                    Pass::Binarized {
                        dark_is_black: true,
                    },
                    Pass::Luma { dark_is_low: true },
                ] {
                    if matches!(pass, Pass::Binarized { .. }) && black.black(0, 0).is_none() {
                        continue; // no binarized image without rxing
                    }
                    let grid = Grid::new(&img, &black, &stated, 1, pass, transposed).unwrap();
                    assert_eq!(
                        grid.format_words(),
                        [masked ^ 0x5412; 2],
                        "{ec:?} mask {mask} T{transposed} {pass:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn either_version_copy_states_the_version_on_its_own() {
        // versions 7-40, one copy blanked (all light: eight bits or more from
        // every version word): the other still states the version; both
        // blanked, none does
        for version in 7i16..=40 {
            let (colors, width) =
                symbol_colors(version, qrcode::EcLevel::L, 0, CASES[0].2).unwrap();
            // the 6 × 3 block left of the top-right finder's separator, and
            // its transpose above the bottom-left one
            let top_right =
                |row: usize, column: usize| row < 6 && (width - 11..width - 8).contains(&column);
            let bottom_left =
                |row: usize, column: usize| column < 6 && (width - 11..width - 8).contains(&row);
            let stated = u8::try_from(version).unwrap();
            for (blank_top_right, blank_bottom_left, expected) in [
                (true, false, Some(stated)),
                (false, true, Some(stated)),
                (true, true, None),
            ] {
                let mut modules = colors.clone();
                for (i, module) in modules.iter_mut().enumerate() {
                    let (row, column) = (i / width, i % width);
                    if (blank_top_right && top_right(row, column))
                        || (blank_bottom_left && bottom_left(row, column))
                    {
                        *module = qrcode::Color::Light;
                    }
                }
                let (img, points) = raster(&modules, width, false).unwrap();
                let black = Binarized::of(&img);
                let grid = Grid::new(
                    &img,
                    &black,
                    &hint(points, EcLevel::L, 1),
                    stated,
                    Pass::Luma { dark_is_low: true },
                    false,
                )
                .unwrap();
                assert_eq!(
                    grid.version_word(),
                    expected,
                    "v{version}, top right blank {blank_top_right}, bottom left blank \
                     {blank_bottom_left}"
                );
            }
        }
    }

    #[test]
    fn side_lengths_is_the_quad_s_perimeter() {
        // sides 3, 4, 3 and 4 in turn, the last one closing on the first point
        let rectangle = [(0.0, 0.0), (3.0, 0.0), (3.0, 4.0), (0.0, 4.0)];
        assert_eq!(side_lengths(&rectangle), 14.0);
        // a rhombus of 3-4-5 sides
        let rhombus = [(1.0, 1.0), (4.0, 5.0), (4.0, 10.0), (1.0, 6.0)];
        assert_eq!(side_lengths(&rhombus), 20.0);
    }

    /// 64 × 64, white with a black 16 × 16 top-left corner and one pixel of
    /// luma 100 at (50, 50) — one whole word per row of rxing's bit matrix,
    /// so the bit right of a row's last pixel is the next row's first.
    fn corner_image() -> LumaImage {
        let mut pixels = vec![255u8; 64 * 64];
        for row in 0..16 {
            pixels[row * 64..row * 64 + 16].fill(0);
        }
        pixels[50 * 64 + 50] = 100;
        LumaImage::new(pixels, 64, 64)
    }

    /// A grid of `size` modules whose module (column, row) is centred on the
    /// pixel (column + offset, row + offset) — exact in f32: 64 cells.
    fn synthetic<'a>(
        img: &'a LumaImage,
        black: &'a Binarized<'a>,
        size: usize,
        offset: f32,
        pass: Pass,
        threshold: f32,
    ) -> Grid<'a> {
        Grid {
            img,
            black,
            to_image: Homography([
                64.0,
                0.0,
                offset - 0.5,
                0.0,
                64.0,
                offset - 0.5,
                0.0,
                0.0,
                1.0,
            ]),
            size,
            cells: 64.0,
            pass,
            mirrored: false,
            threshold,
        }
    }

    #[cfg(feature = "engine-rxing")]
    #[test]
    fn the_binarized_image_answers_within_its_bounds_only() {
        let img = corner_image();
        let black = Binarized::of(&img);
        assert_eq!(black.black(0, 1), Some(true));
        assert_eq!(black.black(15, 15), Some(true));
        assert_eq!(black.black(16, 0), Some(false));
        assert_eq!(black.black(40, 40), Some(false));
        // past the right edge — where the next row's black pixels lie in the
        // bit matrix — and past the bottom: white
        assert_eq!(black.black(64, 0), Some(false));
        assert_eq!(black.black(70, 0), Some(false));
        assert_eq!(black.black(0, 64), Some(false));
    }

    #[cfg(feature = "engine-rxing")]
    #[test]
    fn a_binarized_module_is_dark_by_its_polarity_and_off_the_image_light() {
        let img = corner_image();
        let black = Binarized::of(&img);
        // module (column, row) at pixel (column − 1, row − 1); dark(row, column)
        let grid = |dark_is_black| {
            synthetic(
                &img,
                &black,
                64,
                -1.0,
                Pass::Binarized { dark_is_black },
                0.0,
            )
        };
        let normal = grid(true);
        assert!(normal.dark(1, 1), "pixel (0, 0) is black");
        assert!(normal.dark(6, 6), "pixel (5, 5) is black");
        assert!(!normal.dark(41, 41), "pixel (40, 40) is white");
        // left of or above the image: light — never pixel 0's colour
        assert!(!normal.dark(1, 0), "pixel (−1, 0)");
        assert!(!normal.dark(0, 1), "pixel (0, −1)");
        assert!(!normal.dark(0, 0), "pixel (−1, −1)");
        // the other polarity reads the complement, off the image included
        let inverted = grid(false);
        assert!(!inverted.dark(1, 1) && !inverted.dark(6, 6) && inverted.dark(41, 41));
        assert!(inverted.dark(1, 0) && inverted.dark(0, 1) && inverted.dark(0, 0));
    }

    #[test]
    fn a_luma_module_is_dark_strictly_below_the_threshold() {
        let img = corner_image();
        let black = Binarized::of(&img);
        let grid =
            |dark_is_low| synthetic(&img, &black, 64, -1.0, Pass::Luma { dark_is_low }, 100.0);
        let low = grid(true);
        assert!(low.dark(6, 6), "luma 0 is below 100");
        assert!(!low.dark(51, 51), "luma 100 is not below 100");
        assert!(!low.dark(41, 41), "luma 255");
        let high = grid(false);
        assert!(!high.dark(6, 6) && high.dark(51, 51) && high.dark(41, 41));
    }

    #[test]
    fn the_luma_threshold_is_the_midpoint_of_the_2nd_and_98th_percentiles() {
        // one module per pixel of a `side` × `side` image
        let midpoint = |pixels: Vec<u8>, side: usize| {
            let edge = u32::try_from(side).unwrap();
            let img = LumaImage::new(pixels, edge, edge);
            let black = Binarized::of(&img);
            synthetic(
                &img,
                &black,
                side,
                0.0,
                Pass::Luma { dark_is_low: true },
                0.0,
            )
            .midpoint()
        };
        // 441 samples: the percentiles are the 9th lowest and the 9th highest
        let mut spread = [
            vec![10; 9],
            vec![128; 422],
            vec![200, 210, 220],
            vec![230; 7],
        ]
        .concat();
        spread.reverse();
        assert_eq!(midpoint(spread, 21), Some(110.0));
        let halves = [vec![100; 220], vec![200; 221]].concat();
        assert_eq!(midpoint(halves, 21), Some(150.0));
        // a spread of 16 grey levels is the least that splits
        let least = [vec![100; 432], vec![116; 9]].concat();
        assert_eq!(midpoint(least, 21), Some(108.0));
        let narrow = [vec![100; 432], vec![115; 9]].concat();
        assert_eq!(midpoint(narrow, 21), None);
        assert_eq!(midpoint(vec![200; 441], 21), None, "a flat grid");
        // 16 samples are the fewest that decide; 9 decide nothing
        let sixteen = [vec![0; 8], vec![255; 8]].concat();
        assert_eq!(midpoint(sixteen, 4), Some(127.5));
        let nine = [vec![0; 4], vec![255; 5]].concat();
        assert_eq!(midpoint(nine, 3), None);
    }

    #[test]
    fn a_clean_version_1_symbol_costs_one_probe_and_one_correction() {
        // version 1 is tried first: the first pass verifies it there
        for (j, (ec, level)) in INDICATED.into_iter().enumerate() {
            for mask in [j, j + 4] {
                let (img, points) = render(1, ec, mask, false, CASES[0].2).unwrap();
                let (found, spent) = locate_spending(&img, &hint(points, level, 1), b"abc");
                assert!(
                    matches!(found, Located::Parsed { version: 1, .. }),
                    "{ec:?} mask {mask}: {found:?}"
                );
                assert_eq!(
                    spent,
                    Spent {
                        format_probes: 1,
                        rs_attempts: 1
                    },
                    "{ec:?} mask {mask}"
                );
            }
        }
    }

    #[cfg(feature = "engine-rxing")]
    #[test]
    fn a_symbol_only_the_luma_pass_reads_is_read_after_both_binarized_passes() {
        // dark 115 and light 135: the binarizer reads blocks that flat (at
        // most 24 grey levels) as all light, the luma percentiles still split
        // (20 ≥ 16). At 4 px per module the quad admits versions 1-16: both
        // binarized passes probe each as read and transposed — all light,
        // then all dark, no format word within three bits — before the luma
        // pass verifies version 1
        let (img, points) = render(1, qrcode::EcLevel::M, 2, false, CASES[0].2).unwrap();
        let pixels = img
            .data()
            .iter()
            .map(|&p| if p == 0 { 115 } else { 135 })
            .collect();
        let img = LumaImage::new(pixels, img.width(), img.height());
        let (found, spent) = locate_spending(&img, &hint(points, EcLevel::M, 1), b"abc");
        assert!(
            matches!(
                found,
                Located::Parsed {
                    version: 1,
                    mask: 2,
                    ..
                }
            ),
            "{found:?}"
        );
        assert_eq!(
            spent,
            Spent {
                format_probes: 2 * 2 * 16 + 1,
                rs_attempts: 1
            }
        );
    }

    #[test]
    fn a_malformed_stream_is_refused_only_as_the_port_would_read_the_payload() {
        // v1-M: ECI 1000000 (above the register), then byte "abc" — the
        // scanner's parser refuses the stream, rxing's port signals a
        // designator and reads "abc"
        let data = crate::rescue::test_symbols::stream(
            "0111 11001111 01000010 01000000 0100 00000011 01100001 01100010 01100011 0000",
        );
        let img = crate::rescue::test_symbols::from_codewords(
            qrcode::Version::Normal(1),
            qrcode::EcLevel::M,
            &data,
        );
        // the outer corners plus half a pixel: 6 px modules, a 4-module quiet zone
        let (lo, hi) = (24.5, 25.0 * 6.0 + 0.5);
        let points = [
            Point { x: lo, y: lo },
            Point { x: hi, y: lo },
            Point { x: hi, y: hi },
            Point { x: lo, y: hi },
        ];
        let port = PortRead {
            bytes: b"abc".to_vec(),
            signalled: true,
            fnc1: Fnc1::None,
        };
        assert_eq!(
            locate(&img, &hint(points, EcLevel::M, 2), b"abc"),
            Located::Refused { port }
        );
        // other bytes, a modifier that states no designator, or none at all
        // (rqrr): nothing is refused, and nothing verifies
        assert_eq!(
            locate(&img, &hint(points, EcLevel::M, 2), b"abd"),
            Located::Unverified
        );
        assert_eq!(
            locate(&img, &hint(points, EcLevel::M, 1), b"abc"),
            Located::Unverified
        );
        let rqrr = LocateHint {
            modifier: None,
            ..hint(points, EcLevel::M, 2)
        };
        assert_eq!(locate(&img, &rqrr, b"abc"), Located::Unverified);
    }
}
