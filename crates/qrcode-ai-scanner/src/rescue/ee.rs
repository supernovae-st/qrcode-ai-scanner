//! Errors-and-erasures Reed-Solomon correction over the QR field —
//! Forney 1965 (DOI 10.1109/TIT.1965.1053825), ISO/IEC 18004 Annex B.
//!
//! The capacity law: `e + 2t ≤ d − p` (e erasures at known positions cost
//! HALF an unknown error t). The decode engines treat every corrupted
//! codeword as a full-price error and give up at `t > ⌊(d−p)/2⌋`; marking
//! the logo-occluded / low-confidence codewords as erasures is what makes
//! the artistic-QR rescue stage recover past that wall.
//!
//! Convention (matches `score::uec::syndromes`): the block is a polynomial
//! with `block[len−1−j]` as the `x^j` coefficient — "power position" j
//! counts from the LAST byte; generator roots are α^0..α^(npar−1) (b = 0).

use crate::score::uec::{gf_inv, gf_mul, gf_pow, syndromes};

/// Multiply two GF(256) polynomials (ascending-power coefficient vectors).
fn poly_mul(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; a.len() + b.len() - 1];
    for (i, &ca) in a.iter().enumerate() {
        if ca == 0 {
            continue;
        }
        for (j, &cb) in b.iter().enumerate() {
            out[i + j] ^= gf_mul(ca, cb);
        }
    }
    out
}

/// Value of an ascending-power polynomial at `x` (Horner from the top).
fn poly_at(poly: &[u8], x: u8) -> u8 {
    let mut acc = 0u8;
    for &c in poly.iter().rev() {
        acc = gf_mul(acc, x) ^ c;
    }
    acc
}

/// Berlekamp-Massey on (modified) syndromes — returns the error locator
/// Λ(x), ascending powers. Identical recurrence to `uec::error_count` but
/// keeping the polynomial, not just its degree.
fn berlekamp_massey(synd: &[u8]) -> Vec<u8> {
    let len = synd.len();
    let mut locator = vec![0u8; len + 1];
    let mut prev = vec![0u8; len + 1];
    locator[0] = 1;
    prev[0] = 1;
    let mut degree = 0usize;
    let mut shift = 1usize;
    let mut last_discrepancy = 1u8;

    for round in 0..len {
        let mut discrepancy = synd[round];
        for i in 1..=degree.min(round) {
            discrepancy ^= gf_mul(locator[i], synd[round - i]);
        }
        if discrepancy == 0 {
            shift += 1;
        } else if 2 * degree <= round {
            let coef = gf_mul(discrepancy, gf_inv(last_discrepancy));
            let snapshot = locator.clone();
            for i in shift..=len {
                locator[i] ^= gf_mul(coef, prev[i - shift]);
            }
            prev = snapshot;
            degree = round + 1 - degree;
            last_discrepancy = discrepancy;
            shift = 1;
        } else {
            let coef = gf_mul(discrepancy, gf_inv(last_discrepancy));
            for i in shift..=len {
                locator[i] ^= gf_mul(coef, prev[i - shift]);
            }
            shift += 1;
        }
    }
    locator.truncate(degree + 1);
    locator
}

/// Correct one RS block in place. `erasures` are POWER positions (j with
/// `block[len−1−j]` the x^j coefficient) marked unreliable by the caller.
/// Returns `(errors, erasures)` actually corrected on success; `None` when
/// the block is beyond capacity or inconsistent. The final syndrome
/// re-check rejects INCONSISTENT corrections — a miscorrection that lands
/// on another valid codeword passes it by construction (that class is
/// what the UEC margin-0 `low_correction_margin` hint flags downstream).
pub(super) fn correct(block: &mut [u8], npar: usize, erasures: &[usize]) -> Option<(usize, usize)> {
    if npar == 0
        || erasures.len() >= npar
        || block.len() < npar
        || block.is_empty()
        || erasures.iter().any(|&p| p >= block.len())
    {
        return None;
    }
    let synd = syndromes(block, npar);
    if synd.iter().all(|&s| s == 0) {
        return Some((0, 0)); // already clean — erasure marks were spurious
    }

    // Γ(x) = Π (1 − α^{p} x) over the erasure positions
    let mut gamma = vec![1u8];
    for &p in erasures {
        gamma = poly_mul(&gamma, &[1, gf_pow(p)]);
    }
    // Ξ(x) = S(x)·Γ(x) mod x^npar — the erasure-modified syndromes
    let mut xi = poly_mul(&synd, &gamma);
    xi.truncate(npar);

    // Λ(x) from BM on the SHIFTED modified syndromes Ξ[e..]: the first e
    // coefficients of Ξ belong to Ω (deg Ω < e + ν), not to the locator
    // recurrence — the textbook errors-and-erasures subtlety. Combined
    // locator Ψ = Λ·Γ.
    let lambda = berlekamp_massey(&xi[erasures.len()..]);
    let nu = lambda.len() - 1; // unknown-error count
    if 2 * nu + erasures.len() > npar {
        return None; // capacity exceeded — refuse, never guess
    }
    let psi = poly_mul(&lambda, &gamma);

    // Chien search over the actual block length: position j is corrupted
    // iff Ψ(α^{−j}) = 0
    let mut positions = Vec::new();
    for j in 0..block.len() {
        if poly_at(&psi, gf_inv(gf_pow(j))) == 0 {
            positions.push(j);
        }
    }
    // every locator root must land inside the block
    if positions.len() != psi.len() - 1 {
        return None;
    }

    // Ω(x) = S(x)·Ψ(x) mod x^npar, then Forney (b = 0):
    // e_j = X_j · Ω(X_j^{−1}) / Ψ'(X_j^{−1})
    let mut omega = poly_mul(&synd, &psi);
    omega.truncate(npar);
    // formal derivative over GF(2^8): even-degree terms vanish
    let psi_prime: Vec<u8> = psi
        .iter()
        .skip(1)
        .enumerate()
        .map(|(k, &c)| if k % 2 == 0 { c } else { 0 })
        .collect();

    for &j in &positions {
        let x = gf_pow(j);
        let x_inv = gf_inv(x);
        let denom = poly_at(&psi_prime, x_inv);
        if denom == 0 {
            return None;
        }
        let magnitude = gf_mul(x, gf_mul(poly_at(&omega, x_inv), gf_inv(denom)));
        block[block.len() - 1 - j] ^= magnitude;
    }

    // hard verification: the corrected block must BE a codeword
    let check = syndromes(block, npar);
    if check.iter().any(|&s| s != 0) {
        return None;
    }
    Some((nu, erasures.len()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Test-only systematic RS encoder, generator roots α^0..α^(npar−1) —
    /// the exact code the QR spec uses (and the syndromes convention pins).
    fn rs_encode(data: &[u8], npar: usize) -> Vec<u8> {
        // g(x) = Π (x + α^i), built ascending then used descending
        let mut generator = vec![1u8];
        for i in 0..npar {
            generator = poly_mul(&generator, &[gf_pow(i), 1]);
        }
        // long division of data·x^npar by g(x) in descending-coefficient form
        let mut msg = data.to_vec();
        msg.extend(std::iter::repeat_n(0u8, npar));
        for i in 0..data.len() {
            let coef = msg[i];
            if coef == 0 {
                continue;
            }
            for (k, &g) in generator.iter().rev().enumerate() {
                msg[i + k] ^= gf_mul(coef, g);
            }
        }
        let mut out = data.to_vec();
        out.extend_from_slice(&msg[data.len()..]);
        out
    }

    #[test]
    fn encoder_self_check_and_pure_error_correction() {
        let data = b"hello rescue stage!!";
        let block = rs_encode(data, 10);
        assert!(
            syndromes(&block, 10).iter().all(|&s| s == 0),
            "encoder must emit a codeword"
        );

        // 5 unknown errors = exactly t_max for npar 10
        let mut corrupted = block.clone();
        for (i, delta) in [
            (0usize, 0x5A),
            (7, 0x01),
            (13, 0xFF),
            (20, 0x80),
            (24, 0x33),
        ] {
            corrupted[i] ^= delta;
        }
        let (nu, e) = correct(&mut corrupted, 10, &[]).expect("t = npar/2 must correct");
        assert_eq!((nu, e), (5, 0));
        assert_eq!(corrupted, block);
    }

    #[test]
    fn erasures_double_the_budget_past_the_error_wall() {
        let data = b"the artistic logo zone";
        let npar = 10;
        let block = rs_encode(data, npar);
        let len = block.len();

        // 8 corrupted codewords: t=8 > t_max=5 — plain RS is dead…
        let hit = [1usize, 4, 9, 12, 15, 18, 22, 25];
        let mut corrupted = block.clone();
        for &i in &hit {
            corrupted[i] ^= 0xA7;
        }
        assert!(
            correct(&mut corrupted.clone(), npar, &[]).is_none(),
            "8 errors past the t_max=5 wall must fail without erasures"
        );

        // …but with all 8 MARKED as erasures: e=8 ≤ d=10 → recovers
        let marks: Vec<usize> = hit.iter().map(|&i| len - 1 - i).collect();
        let (nu, e) = correct(&mut corrupted, npar, &marks).expect("e = 8 ≤ d = 10 must correct");
        assert_eq!((nu, e), (0, 8));
        assert_eq!(corrupted, block);
    }

    #[test]
    fn mixed_errors_and_erasures_respect_the_capacity_law() {
        let data = b"mixed budget";
        let npar = 12;
        let block = rs_encode(data, npar);
        let len = block.len();

        // e=6 + 2·t=3 → 12 = npar: exactly at capacity
        let erased = [0usize, 2, 4, 6, 8, 10];
        let errored = [12usize, 15, 19];
        let mut corrupted = block.clone();
        for &i in erased.iter().chain(&errored) {
            corrupted[i] ^= 0x3C;
        }
        let marks: Vec<usize> = erased.iter().map(|&i| len - 1 - i).collect();
        let (nu, e) = correct(&mut corrupted, npar, &marks).expect("at exact capacity");
        assert_eq!((nu, e), (3, 6));
        assert_eq!(corrupted, block);

        // one more unknown error breaks the law → must REFUSE, not guess
        let mut over = block.clone();
        for &i in erased.iter().chain(&[12usize, 15, 19, 21]) {
            over[i] ^= 0x3C;
        }
        assert!(correct(&mut over, npar, &marks).is_none());
    }

    #[test]
    fn spurious_erasure_marks_on_a_clean_block_are_harmless() {
        let block = rs_encode(b"clean", 8);
        let mut copy = block.clone();
        let (nu, e) = correct(&mut copy, 8, &[0, 3, 5]).expect("clean stays clean");
        assert_eq!((nu, e), (0, 0));
        assert_eq!(copy, block);
    }

    #[test]
    fn erasure_overflow_and_degenerate_inputs_refuse() {
        let block = rs_encode(b"xyzw", 4);
        assert!(correct(&mut block.clone(), 4, &[0, 1, 2, 3]).is_none()); // e ≥ npar
        assert!(correct(&mut block.clone(), 4, &[99]).is_none()); // out of range
        assert!(correct(&mut [], 4, &[]).is_none());
        let mut b = block.clone();
        assert!(correct(&mut b, 0, &[]).is_none());
    }

    proptest::proptest! {
        /// Roundtrip law: any corruption within e + 2t ≤ npar, with the
        /// erased positions marked, must restore the exact codeword.
        #[test]
        #[expect(clippy::cast_possible_truncation, reason = "test-only seed folding")]
        fn within_capacity_always_recovers(seed in 0u64..5_000, npar in 4usize..=16) {
            #[expect(clippy::cast_possible_truncation, reason = "test-only seed folding")]
            let data: Vec<u8> =
                (0..30u8).map(|i| i.wrapping_mul(31).wrapping_add(seed as u8)).collect();
            let block = rs_encode(&data, npar);
            let len = block.len();

            let max_e = (seed as usize) % npar; // 0..npar-1
            let t = ((seed / 7) as usize) % ((npar - max_e) / 2 + 1);

            let mut corrupted = block.clone();
            let mut marks = Vec::new();
            let mut pos = (seed as usize) % len;
            for k in 0..max_e {
                pos = (pos + 7 + k) % len;
                while marks.contains(&(len - 1 - pos)) {
                    pos = (pos + 1) % len;
                }
                #[expect(clippy::cast_possible_truncation, reason = "test-only")]
                {
                    corrupted[pos] ^= 0x11u8.wrapping_add(k as u8).max(1);
                }
                marks.push(len - 1 - pos);
            }
            let mut hits = Vec::new();
            for k in 0..t {
                pos = (pos + 13 + k) % len;
                while marks.contains(&(len - 1 - pos)) || hits.contains(&pos) {
                    pos = (pos + 1) % len;
                }
                #[expect(clippy::cast_possible_truncation, reason = "test-only")]
                {
                    corrupted[pos] ^= 0x55u8.wrapping_add(k as u8).max(1);
                }
                hits.push(pos);
            }

            let restored = correct(&mut corrupted, npar, &marks);
            proptest::prop_assert!(restored.is_some(), "e={} t={} npar={}", max_e, t, npar);
            proptest::prop_assert_eq!(corrupted, block);
        }
    }
}
