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

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_possible_truncation)]

    use super::*;

    /// The `GF_LOG` build loop bound is `while i < 255` (gf256.rs 31:13). It is
    /// deliberately `< 255`, NOT `<= 255`: exponent 255 is where α wraps
    /// (α^255 = α^0 = 1), so a `<`→`<=` swap runs one extra iteration that
    /// overwrites `log[GF_EXP[255]] = log[1]` from its correct value 0 to 255.
    /// Downstream that difference is masked (every consumer reduces the log mod
    /// 255, and 255 ≡ 0), so no field OPERATION distinguishes it — the table
    /// value itself is the discriminator. log(1) = 0 is the discrete-log
    /// definition of the multiplicative identity.
    #[test]
    fn log_of_one_is_zero_not_the_wrapped_exponent() {
        assert_eq!(
            GF_LOG[1], 0,
            "log(1) must be 0 — loop bound `< 255` is load-bearing"
        );
    }

    /// Field self-consistency from first principles (independent of the tables'
    /// own construction expressions): exp∘log = identity on the 255 nonzero
    /// elements, and a·a⁻¹ = 1.
    #[test]
    fn exp_log_are_inverses_and_gf_inv_is_a_true_inverse() {
        for a in 1u16..=255 {
            let a = a as u8;
            assert_eq!(
                GF_EXP[GF_LOG[a as usize] as usize], a,
                "exp(log({a})) != {a}"
            );
            assert_eq!(gf_mul(a, gf_inv(a)), 1, "a·a⁻¹ != 1 for a={a}");
        }
    }
}
