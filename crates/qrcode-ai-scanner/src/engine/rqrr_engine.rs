//! rqrr wrapper — the geometry + metadata engine (quirc lineage).
//!
//! Always through `decode_to` raw bytes: rqrr's `decode()` forces UTF-8 and
//! silently discards ECI (verified in rqrr 0.10.1 source) — charset is OUR
//! job, once, in `engine::charset`.

use super::RawDetection;
use crate::input::LumaImage;
use crate::report::{EcLevel, EngineKind, Point};

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

pub(super) fn decode(luma: &LumaImage) -> Vec<RawDetection> {
    let width = luma.width() as usize;
    let data = luma.data();
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(width, luma.height() as usize, |x, y| {
            data[y * width + x]
        });

    let mut found = Vec::new();
    for grid in prepared.detect_grids() {
        let mut raw = Vec::new();
        let Ok(meta) = grid.decode_to(&mut raw) else {
            continue;
        };
        let corners = grid.bounds.map(|p| Point {
            x: p.x as f32,
            y: p.y as f32,
        });
        found.push(RawDetection {
            raw,
            text_hint: None,
            corners: Some(corners),
            version: u8::try_from(meta.version.0).ok(),
            ec: map_ec(meta.ecc_level),
            mask: u8::try_from(meta.mask).ok(),
            engine: EngineKind::Rqrr,
        });
    }
    found
}
