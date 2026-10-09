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
//! Bounded and deterministic: versions 1-40 (an rqrr read: its measured
//! version) × {as read, transposed} × three sampler passes — at most
//! [`MAX_PROBES`] format probes and as many error-correction attempts per
//! call; no clock, no RNG, no budget. Score cells and probes never run it.

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

/// How a read's corner points frame the module grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// The version the engine measured (rqrr); `None`: try every version.
    pub(crate) version: Option<u8>,
    /// The AIM modifier digit (`]Qn`) the engine stated; `None` for rqrr,
    /// which decodes neither FNC1 mode and states nothing of ECI.
    pub(crate) modifier: Option<u8>,
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
    let versions: Vec<u8> = match hint.version {
        Some(version) if (1..=40).contains(&version) => vec![version],
        Some(_) => Vec::new(),
        None => (1..=40).collect(),
    };
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
                    Rederived::Parsed(parsed)
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
