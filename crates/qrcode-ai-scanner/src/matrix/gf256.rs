//! GF(256) arithmetic over the QR field — poly 0x11D, generator α = 2.
//!
//! Log/antilog tables + the field operations (multiply, power, inverse) and
//! Reed-Solomon syndromes. Shared substrate: the UEC error-count path
//! (`score::uec`) and the errors-and-erasures rescue corrector (`rescue::ee`)
//! both build on these — the field never depends on either.

#[expect(
    clippy::cast_possible_truncation,
    reason = "value is reduced mod 0x11D before the cast — always < 256"
)]
const GF_EXP: [u8; 256] = {
    let mut exp = [0u8; 256];
    let mut value: u16 = 1;
    let mut i = 0;
    while i < 256 {
        exp[i] = value as u8;
        value <<= 1;
        if value & 0x100 != 0 {
            value ^= 0x11D;
        }
        i += 1;
    }
    exp
};

#[expect(clippy::cast_possible_truncation, reason = "i < 255")]
const GF_LOG: [u8; 256] = {
    let mut log = [0u8; 256];
    let mut i = 0;
    while i < 255 {
        log[GF_EXP[i] as usize] = i as u8;
        i += 1;
    }
    log
};

pub(crate) fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    let idx = (usize::from(GF_LOG[a as usize]) + usize::from(GF_LOG[b as usize])) % 255;
    GF_EXP[idx]
}

/// α^n.
pub(crate) fn gf_pow(n: usize) -> u8 {
    GF_EXP[n % 255]
}

/// RS syndromes `S_i = c(α^i)` for `i ∈ 0..npar`, with `block[len−1−j]`
/// the coefficient of `x^j` (transmission order) — rqrr convention.
pub(crate) fn syndromes(block: &[u8], npar: usize) -> Vec<u8> {
    let mut s = vec![0u8; npar];
    for (i, slot) in s.iter_mut().enumerate() {
        for j in 0..block.len() {
            let c = block[block.len() - 1 - j];
            *slot ^= gf_mul(c, gf_pow(i * j));
        }
    }
    s
}

pub(crate) fn gf_inv(a: u8) -> u8 {
    if a == 0 {
        return 0;
    }
    GF_EXP[(255 - usize::from(GF_LOG[a as usize])) % 255]
}
