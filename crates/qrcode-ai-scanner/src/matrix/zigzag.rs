//! Module-placement walk — the deterministic zigzag traversal + mask + unmask
//! that recovers each data bit's module position from a still-masked stream.
//!
//! Mirrors rqrr 0.10.1 `decode.rs` semantics (quirc heritage · Apache-2.0/MIT):
//! `read_data`'s down-up column pairs from the bottom-right (column 6 skipped),
//! the function-pattern map (`reserved_cell`), and the 8 ISO mask formulas.
//! Shared substrate: the UEC margin (`score::uec`) and the artistic rescue
//! stage (`rescue`) both replay this walk.

use super::version_db::VERSIONS;

/// Function-pattern map — mirrors rqrr `reserved_cell` exactly.
fn reserved_cell(version: usize, i: usize, j: usize) -> bool {
    debug_assert!(
        (1..=40).contains(&version),
        "reserved_cell: QR version {version} out of 1..=40"
    );
    let ver = &VERSIONS[version];
    let size = version * 4 + 17;

    // finder + format: top-left · bottom-left · top-right
    if j < 9 && (i < 9 || i + 8 >= size) {
        return true;
    }
    if i < 9 && j + 8 >= size {
        return true;
    }
    // timing patterns
    if i == 6 || j == 6 {
        return true;
    }
    // version info (v7+)
    if version >= 7 && ((i < 6 && j + 11 >= size) || (i + 11 >= size && j < 6)) {
        return true;
    }
    // alignment patterns
    let mut ai = None;
    let mut aj = None;
    let mut len = 0;
    for (a, &pattern) in ver.apat.iter().take_while(|&&p| p != 0).enumerate() {
        len = a;
        if pattern.abs_diff(i) < 3 {
            ai = Some(a);
        }
        if pattern.abs_diff(j) < 3 {
            aj = Some(a);
        }
    }
    match (ai, aj) {
        (Some(x), Some(y)) if x == len && y == len => true,
        (Some(x), Some(_)) if 0 < x && x < len => true,
        (Some(_), Some(y)) if 0 < y && y < len => true,
        _ => false,
    }
}

/// Mask predicate — mirrors rqrr `mask_bit` (the 8 ISO formulas).
fn mask_bit(mask: u8, y: usize, x: usize) -> bool {
    match mask {
        0 => (y + x).is_multiple_of(2),
        1 => y.is_multiple_of(2),
        2 => x.is_multiple_of(3),
        3 => (y + x).is_multiple_of(3),
        4 => ((y / 2) + (x / 3)).is_multiple_of(2),
        5 => (y * x) % 2 + (y * x) % 3 == 0,
        6 => ((y * x) % 2 + (y * x) % 3).is_multiple_of(2),
        7 => ((y * x) % 3 + (y + x) % 2).is_multiple_of(2),
        _ => false,
    }
}

/// Module position of every emitted data bit, in stream order — replays
/// rqrr `read_data`'s zigzag exactly (down-up column pairs from the
/// bottom-right, column 6 skipped).
pub(crate) fn zigzag_positions(version: usize) -> Vec<(usize, usize)> {
    debug_assert!(
        (1..=40).contains(&version),
        "zigzag_positions: QR version {version} out of 1..=40"
    );
    let size = version * 4 + 17;
    let mut positions = Vec::with_capacity(VERSIONS[version].total * 8);
    let mut y = size - 1;
    let mut x = size - 1;
    let mut neg_dir = true;
    while x > 0 {
        if x == 6 {
            x -= 1;
        }
        if !reserved_cell(version, y, x) {
            positions.push((y, x));
        }
        if !reserved_cell(version, y, x - 1) {
            positions.push((y, x - 1));
        }
        let (new_y, new_neg_dir) = match (y, neg_dir) {
            (0, true) => {
                x = x.saturating_sub(2);
                (0, false)
            }
            (yy, false) if yy == size - 1 => {
                x = x.saturating_sub(2);
                (size - 1, true)
            }
            (yy, true) => (yy - 1, true),
            (yy, false) => (yy + 1, false),
        };
        y = new_y;
        neg_dir = new_neg_dir;
    }
    positions
}

/// Unmask the still-masked stream into interleaved codeword bytes.
/// Returns `None` when the stream is shorter than the version's capacity.
pub(crate) fn unmask_to_codewords(
    masked: &[u8],
    bit_len: usize,
    version: usize,
    mask: u8,
) -> Option<Vec<u8>> {
    let total = VERSIONS[version].total;
    // BOTH the declared bit length AND the actual buffer must cover the
    // symbol: bit_len alone is caller-supplied and the loop below indexes
    // the buffer (doc promise: short stream ⇒ None, never garbage/panic).
    if bit_len < total * 8 || masked.len() < total {
        return None;
    }
    let positions = zigzag_positions(version);
    let mut codewords = vec![0u8; total];
    for (k, &(y, x)) in positions.iter().take(total * 8).enumerate() {
        // rqrr RawData packing: MSB-first within each byte
        let mut bit = (masked[k / 8] >> (7 - (k % 8))) & 1;
        if mask_bit(mask, y, x) {
            bit ^= 1;
        }
        codewords[k / 8] |= bit << (7 - (k % 8));
    }
    Some(codewords)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The finder/format reservation `if j < 9 && (i < 9 || …)` is STRICT: the
    /// finder + format box spans modules 0..=8, so row/column 9 is the first
    /// DATA cell. A `< → <=` slip would wrongly reserve module row/column 9,
    /// dropping it from every zigzag position → a scrambled bitstream. Pin both
    /// `<` with cells exactly AT the boundary that v5 leaves free (its alignment
    /// centre sits at 6/30, clear of these cells).
    #[test]
    fn reserved_cell_finder_bounds_are_strict() {
        // column 9 (j=9): `j < 9` is false → free. `j <= 9` would reserve it.
        assert!(!reserved_cell(5, 0, 9), "column 9 is data, not reserved");
        // row 9 (i=9): the inner `i < 9` is false → free. `i <= 9` reserves it.
        assert!(!reserved_cell(5, 9, 0), "row 9 is data, not reserved");
        // sanity on the TRUE side: (8,8) is the far corner of the box.
        assert!(reserved_cell(5, 8, 8), "the finder+format box is reserved");
    }

    /// Hand-computed ISO 18004 mask-formula truth table — the pristine
    /// matrix only exercises whichever masks the generator picked; mutation
    /// testing showed masks 1 and 6 had no kill coverage.
    #[test]
    fn mask_formulas_truth_table() {
        // (mask, y, x, expected)
        let table: [(u8, usize, usize, bool); 24] = [
            (0, 0, 0, true),
            (0, 0, 1, false),
            (0, 1, 2, false),
            (0, 2, 2, true),
            (1, 0, 5, true),
            (1, 1, 5, false),
            (1, 2, 0, true),
            (2, 0, 0, true),
            (2, 0, 1, false),
            (2, 0, 3, true),
            (3, 0, 0, true),
            (3, 1, 2, true),
            (3, 2, 2, false),
            (4, 0, 0, true),
            (4, 2, 0, false),
            (4, 2, 3, true),
            (5, 0, 4, true),
            (5, 1, 1, false),
            (5, 2, 3, true),
            (6, 1, 1, true),
            (6, 1, 5, false),
            (6, 2, 3, true),
            (7, 0, 1, false),
            (7, 1, 5, true),
        ];
        for (mask, y, x, expected) in table {
            assert_eq!(
                mask_bit(mask, y, x),
                expected,
                "mask {mask} at (y={y}, x={x})"
            );
        }
        assert!(!mask_bit(8, 0, 0), "unknown mask is never set");
    }

    /// The masks 5/6/7 use MODULO, not addition — a `% → +` slip changes the
    /// value fed to the `== 0` / `is_multiple_of(2)` test. Each assertion below
    /// straddles the mutant. (Two of the mask-6/7 `% 2 → + 2` slips are proven
    /// EQUIVALENT: `n + 2 ≡ n (mod 2)` so the parity feeding `is_multiple_of(2)`
    /// is unchanged — those are justified in the commit, not pinned here.)
    #[test]
    fn mask_bit_uses_modulo_not_addition() {
        // mask 5 ≡ (y·x ≡ 0 mod 6). Both `% 2 → + 2` and `% 3 → + 3` push the
        // LHS to ≥ 2, so it can never equal 0 → every TRUE case dies. y·x = 6.
        assert!(mask_bit(5, 2, 3), "y·x=6 → 6%2 + 6%3 = 0 → set");
        assert!(mask_bit(5, 1, 6), "y·x=6 → set");
        assert!(!mask_bit(5, 1, 1), "y·x=1 → 1%2 + 1%3 = 2 → clear");
        // mask 6: `(y·x)%3 → (y·x)+3` flips the parity into is_multiple_of(2).
        // y·x=1: (1%2 + 1%3)=2 even → set; mutant (1 + (1+3))=5 odd → clear.
        assert!(mask_bit(6, 1, 1), "1%2 + 1%3 = 2 even → set");
        // mask 7: `(y·x)%3 → (y·x)+3` flips the parity. y·x=1, y+x=2:
        // (1%3 + 2%2)=(1+0)=1 odd → clear; mutant ((1+3)+0)=4 even → set.
        assert!(!mask_bit(7, 1, 1), "1%3 + 2%2 = 1 odd → clear");
    }
}
