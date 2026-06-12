//! rqrr wrapper — the geometry + metadata engine (quirc lineage).
//!
//! Always through `decode_to` raw bytes: rqrr's `decode()` forces UTF-8 and
//! silently discards ECI (verified in rqrr 0.10.1 source) — charset is OUR
//! job, once, in `engine::charset`.

use super::{MaskedStream, RawDetection};
use crate::input::LumaImage;
use crate::report::{EcLevel, EngineKind, Point};
use crate::rescue::RescueCandidate;

/// rqrr `ecc_level` is the QR format-info two-bit field: M=0 · L=1 · H=2 · Q=3.
/// Pinned empirically by the engine tests (generated at known EC levels).
fn map_ec(level: u16) -> Option<EcLevel> {
    match level {
        0 => Some(EcLevel::M),
        1 => Some(EcLevel::L),
        2 => Some(EcLevel::H),
        3 => Some(EcLevel::Q),
        _ => None,
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "corner coordinates are bounded by Limits::max_dimension (10k) — exact in f32 (2^24)"
)]
pub(super) fn decode(luma: &LumaImage) -> (Vec<RawDetection>, Vec<RescueCandidate>) {
    let width = luma.width() as usize;
    let data = luma.data();
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(width, luma.height() as usize, |x, y| {
            data[y * width + x]
        });

    let mut found = Vec::new();
    let mut rescue = Vec::new();
    for grid in prepared.detect_grids() {
        let mut raw = Vec::new();
        let Ok(meta) = grid.decode_to(&mut raw) else {
            // RS gave up — but the grid + format info may still read: that
            // stream is the S5 erasure-rescue input
            if let Ok((meta, raw_data)) = grid.get_raw_data()
                && let Ok(version) = u8::try_from(meta.version.0)
                && let Some(ec) = map_ec(meta.ecc_level)
                && let Ok(mask) = u8::try_from(meta.mask)
            {
                rescue.push(RescueCandidate {
                    stream: MaskedStream {
                        bits: raw_data.data[..raw_data.len.div_ceil(8)].to_vec(),
                        bit_len: raw_data.len,
                    },
                    corners: grid.bounds.map(|p| Point {
                        x: p.x as f32,
                        y: p.y as f32,
                    }),
                    version,
                    ec,
                    mask,
                    inverted: false, // stamped by the ladder per attempt
                });
            }
            continue;
        };
        // raw sampled bitstream (still masked) — the synthetic-UEC input
        let masked_stream = grid.get_raw_data().ok().map(|(_, raw_data)| MaskedStream {
            bits: raw_data.data[..raw_data.len.div_ceil(8)].to_vec(),
            bit_len: raw_data.len,
        });
        let corners = grid.bounds.map(|p| Point {
            x: p.x as f32,
            y: p.y as f32,
        });
        found.push(RawDetection {
            raw,
            masked_stream,
            corners: Some(corners),
            version: u8::try_from(meta.version.0).ok(),
            ec: map_ec(meta.ecc_level),
            mask: u8::try_from(meta.mask).ok(),
            // rqrr cannot decode FNC1 symbols (mode dispatch rejects 0x5/0x9)
            fnc1: false,
            engine: EngineKind::Rqrr,
        });
    }
    (found, rescue)
}
