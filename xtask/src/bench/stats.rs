//! Deterministic statistics for `xtask bench` — no RNG, no resampling.
//!
//! - medians and nearest-rank percentiles of per-image latencies;
//! - the paired comparison: per-image log-ratios, their Hodges-Lehmann
//!   estimate and the exact Wilcoxon signed-rank 95 % interval (Walsh
//!   averages, Hollander & Wolfe), judged against a δ table;
//! - the exact one-sided [`McNemar`] test of a paired binary status
//!   (truncation, lost judgments), decided in exact integers;
//! - the Wilson score 95 % interval for proportions (agreement and
//!   budget-overrun rates).

/// z of a two-sided 95 % interval (the oracle's constant).
const Z95: f64 = 1.959_963_984_540_054;
/// Two-sided 95 %: each tail of the signed-rank distribution holds ≤ 2.5 %.
const TAIL: f64 = 0.025;
/// The smallest paired sample with a 95 % signed-rank interval: with five
/// pairs P(T⁺ = 0) = 1/32 already exceeds the 2.5 % tail.
pub(crate) const MIN_PAIRS: usize = 6;
/// The p95 verdict's tail is the k = ⌊n/10⌋ pairs of a class with the
/// largest per-image geometric mean; with k ≥ 6 required, a class under 60
/// images is not testable.
pub(crate) const TAIL_DIVISOR: usize = 10;

/// k = ⌊n/10⌋, the tail size of a class of `n` pairs.
pub(crate) fn tail_size(n: usize) -> usize {
    n / TAIL_DIVISOR
}

/// Median (mean of the two middle values for an even count).
pub(crate) fn median(values: &[f64]) -> Option<f64> {
    let sorted = sorted(values);
    let n = sorted.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(sorted[n / 2]),
        _ => Some(f64::midpoint(sorted[n / 2 - 1], sorted[n / 2])),
    }
}

/// 1-based nearest rank of the `pct`-th percentile in a sample of `n`:
/// the smallest rank with at least `pct` % of the sample at or below it.
/// Integer arithmetic, so a rank never depends on float rounding.
pub(crate) fn nearest_rank(n: usize, pct: usize) -> usize {
    (pct * n).div_ceil(100).clamp(1, n.max(1))
}

/// Nearest-rank percentile — always an observed value.
pub(crate) fn percentile(values: &[f64], pct: usize) -> Option<f64> {
    let sorted = sorted(values);
    (!sorted.is_empty()).then(|| sorted[nearest_rank(sorted.len(), pct) - 1])
}

fn sorted(values: &[f64]) -> Vec<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted
}

/// p50 / p95 / max of a class (nearest rank).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Spread {
    pub(crate) n: usize,
    pub(crate) p50: f64,
    pub(crate) p95: f64,
    pub(crate) max: f64,
}

pub(crate) fn spread(values: &[f64]) -> Option<Spread> {
    Some(Spread {
        n: values.len(),
        p50: percentile(values, 50)?,
        p95: percentile(values, 95)?,
        max: percentile(values, 100)?,
    })
}

/// P(T⁺ ≤ t) for t = 0..=n(n+1)/2 under the null of the Wilcoxon
/// signed-rank statistic — the exact distribution, as halved subset-sum
/// counts (every step keeps the values in [0, 1]; same operations every
/// run, so the cut points are deterministic).
pub(crate) fn signed_rank_cdf(n: usize) -> Vec<f64> {
    let total = n * (n + 1) / 2;
    let mut mass = vec![0.0f64; total + 1];
    mass[0] = 1.0;
    for i in 1..=n {
        let top = i * (i + 1) / 2;
        for t in (0..=top).rev() {
            let with_i = if t >= i { mass[t - i] } else { 0.0 };
            mass[t] = f64::midpoint(mass[t], with_i);
        }
    }
    let mut acc = 0.0;
    mass.iter()
        .map(|p| {
            acc += p;
            acc
        })
        .collect()
}

/// 1-based rank `k` of the lower 95 % limit among the sorted Walsh
/// averages: the largest k with P(T⁺ ≤ k − 1) ≤ 2.5 %. `None` below
/// [`MIN_PAIRS`]. The interval is [W₍ₖ₎, W₍M+1−k₎], M = n(n+1)/2.
pub(crate) fn signed_rank_k(n: usize) -> Option<usize> {
    if n < MIN_PAIRS {
        return None;
    }
    let cdf = signed_rank_cdf(n);
    let below = cdf.iter().take_while(|&&p| p <= TAIL).count();
    (below > 0).then_some(below)
}

/// Hodges-Lehmann estimate of a paired shift and its exact 95 % interval.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Shift {
    pub(crate) n: usize,
    /// Median of the Walsh averages.
    pub(crate) estimate: f64,
    /// [lower, upper]; `None` below [`MIN_PAIRS`].
    pub(crate) interval: Option<[f64; 2]>,
    /// 1 − 2·P(T⁺ ≤ k − 1): the interval's exact coverage.
    pub(crate) confidence: Option<f64>,
}

/// Hodges-Lehmann on paired differences (here: log-ratios). Ties and
/// zero differences stay in the Walsh averages (no zero-dropping), so the
/// estimate is the plain one-sample HL.
pub(crate) fn hodges_lehmann(diffs: &[f64]) -> Option<Shift> {
    let n = diffs.len();
    let mut walsh = Vec::with_capacity(n * (n + 1) / 2);
    for (index, first) in diffs.iter().enumerate() {
        for second in &diffs[index..] {
            walsh.push(f64::midpoint(*first, *second));
        }
    }
    walsh.sort_by(f64::total_cmp);
    let estimate = median(&walsh)?;
    let rank = signed_rank_k(n);
    let interval = rank.map(|k| [walsh[k - 1], walsh[walsh.len() - k]]);
    let confidence = rank.map(|k| 1.0 - 2.0 * signed_rank_cdf(n)[k - 1]);
    Some(Shift {
        n,
        estimate,
        interval,
        confidence,
    })
}

/// Per-image log-ratio ln(candidate / base); pairs with a non-positive
/// side carry no ratio and are dropped ([`nonpositive`] counts them).
pub(crate) fn log_ratios(pairs: &[(f64, f64)]) -> Vec<f64> {
    pairs
        .iter()
        .filter(|(a, b)| *a > 0.0 && *b > 0.0)
        .map(|(a, b)| (b / a).ln())
        .collect()
}

/// Pairs [`log_ratios`] drops: a side at zero (or below) has no ratio.
pub(crate) fn nonpositive(pairs: &[(f64, f64)]) -> usize {
    pairs.iter().filter(|(a, b)| *a <= 0.0 || *b <= 0.0).count()
}

/// The class tail for a p95 (or memory tail) verdict: the ⌊n/10⌋ pairs
/// with the largest geometric mean √(base·candidate). Ranking by the mean
/// of both sides is symmetric, so choosing the tail favours neither; ties
/// keep input order (a stable sort). Ascending.
pub(crate) fn tail_pairs(pairs: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut ranked: Vec<(f64, (f64, f64))> = pairs
        .iter()
        .copied()
        .filter(|(base, candidate)| *base > 0.0 && *candidate > 0.0)
        .map(|pair| ((pair.0 * pair.1).sqrt(), pair))
        .collect();
    ranked.sort_by(|x, y| x.0.total_cmp(&y.0));
    let keep = tail_size(ranked.len());
    ranked
        .split_off(ranked.len() - keep)
        .into_iter()
        .map(|(_, pair)| pair)
        .collect()
}

/// A verdict: a δ verdict on a log-scale interval, or a [`McNemar`] test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The interval's lower bound exceeds ln(1 + δ) — or, for work, its
    /// upper bound lies below ln(1 − δ); for a [`McNemar`] test, p < α.
    Regression,
    /// An interval exists and does not cross the δ line; the test ran and
    /// p ≥ α.
    NoRegression,
    /// No interval, or fewer than [`MIN_PAIRS`] pairs — never a pass.
    NotTestable,
    /// A resource failure in the class: no verdict stands.
    Failed,
}

impl Verdict {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Regression => "regression",
            Self::NoRegression => "no_regression",
            Self::NotTestable => "not_testable",
            Self::Failed => "failed",
        }
    }
}

/// The level of the [`McNemar`] tests: p < 1/20.
pub(crate) const ALPHA: f64 = 0.05;
const ALPHA_DENOMINATOR: u64 = 20;

/// Unsigned integers of any size, as little-endian 64-bit limbs: binomial
/// tails stay exact far beyond `u128` (C(381, 190) has 377 bits).
mod exact {
    pub(super) type Big = Vec<u64>;

    /// The low limb and the carry of a 128-bit intermediate.
    fn split(value: u128) -> (u64, u128) {
        let low = u64::try_from(value & u128::from(u64::MAX)).unwrap_or_default();
        (low, value >> 64)
    }

    pub(super) fn add(a: &Big, b: &Big) -> Big {
        let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
        let mut carry = 0u128;
        for i in 0..a.len().max(b.len()) {
            let (low, high) = split(
                u128::from(a.get(i).copied().unwrap_or(0))
                    + u128::from(b.get(i).copied().unwrap_or(0))
                    + carry,
            );
            out.push(low);
            carry = high;
        }
        if carry > 0 {
            out.push(split(carry).0);
        }
        out
    }

    pub(super) fn mul_small(a: &Big, m: u64) -> Big {
        let mut out = Vec::with_capacity(a.len() + 1);
        let mut carry = 0u128;
        for limb in a {
            let (low, high) = split(u128::from(*limb) * u128::from(m) + carry);
            out.push(low);
            carry = high;
        }
        if carry > 0 {
            out.push(split(carry).0);
        }
        out
    }

    pub(super) fn pow2(n: usize) -> Big {
        let mut out = vec![0u64; n / 64 + 1];
        out[n / 64] = 1 << (n % 64);
        out
    }

    fn trimmed(a: &Big) -> &[u64] {
        let len = a.iter().rposition(|limb| *limb != 0).map_or(0, |i| i + 1);
        &a[..len]
    }

    pub(super) fn less(a: &Big, b: &Big) -> bool {
        let (a, b) = (trimmed(a), trimmed(b));
        a.len() < b.len() || (a.len() == b.len() && a.iter().rev().lt(b.iter().rev()))
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "a display value; the decision stays in integers"
    )]
    pub(super) fn to_f64(a: &Big) -> f64 {
        a.iter().rev().fold(0.0, |acc, limb| {
            acc * 18_446_744_073_709_551_616.0 + *limb as f64
        })
    }

    /// Σ C(n, k) for k ≥ `from`, from row `n` of Pascal's triangle.
    pub(super) fn binomial_tail(n: usize, from: usize) -> Big {
        let mut row: Vec<Big> = vec![vec![1]];
        for _ in 0..n {
            let mut next = Vec::with_capacity(row.len() + 1);
            next.push(vec![1]);
            for pair in row.windows(2) {
                next.push(add(&pair[0], &pair[1]));
            }
            next.push(vec![1]);
            row = next;
        }
        row.iter().skip(from).fold(vec![0], |acc, c| add(&acc, c))
    }
}

/// The exact one-sided `McNemar` test of one class: `against` images where
/// only the candidate has the status (truncated, judgment lost), `favour`
/// where only the base has it. Under the null every discordant image is a
/// fair coin, so `against` follows Binomial(against + favour, ½) and
/// p = P(X ≥ against).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct McNemar {
    pub(crate) against: usize,
    pub(crate) favour: usize,
    /// For display; [`McNemar::significant`] is decided in integers.
    pub(crate) p: f64,
    /// p < α, i.e. 20 · Σ C(n, k≥against) < 2ⁿ.
    pub(crate) significant: bool,
}

pub(crate) fn mcnemar(against: usize, favour: usize) -> McNemar {
    let n = against + favour;
    let tail = exact::binomial_tail(n, against);
    let significant = exact::less(&exact::mul_small(&tail, ALPHA_DENOMINATOR), &exact::pow2(n));
    let exponent = i32::try_from(n).unwrap_or(i32::MAX);
    McNemar {
        against,
        favour,
        p: exact::to_f64(&tail) * 2f64.powi(-exponent),
        significant,
    }
}

/// δ as a fraction (0.05 = 5 %); `shift` on the log scale.
pub(crate) fn verdict(shift: Option<&Shift>, delta: f64) -> Verdict {
    match shift.and_then(|s| s.interval) {
        None => Verdict::NotTestable,
        Some([lower, _]) if lower > delta.ln_1p() => Verdict::Regression,
        Some(_) => Verdict::NoRegression,
    }
}

/// The work verdict of a budget-bound class: a regression when the interval
/// of ln(candidate / base) transforms lies wholly below ln(1 − δ) — the
/// candidate fit significantly less work into the same budget.
pub(crate) fn work_verdict(shift: Option<&Shift>, delta: f64) -> Verdict {
    match shift.and_then(|s| s.interval) {
        None => Verdict::NotTestable,
        Some([_, upper]) if upper < (-delta).ln_1p() => Verdict::Regression,
        Some(_) => Verdict::NoRegression,
    }
}

/// A deterministic quantity (an artifact size): regression when the
/// candidate exceeds the base by more than δ, given in basis points and
/// compared in integers — exactly +δ is not a regression (as a float,
/// 1020 / 1000 − 1 lands above 0.02).
pub(crate) fn size_verdict(base: u64, candidate: u64, delta_bp: u64) -> Verdict {
    let over = u128::from(candidate) * 10_000 > u128::from(base) * u128::from(10_000 + delta_bp);
    if base > 0 && over {
        Verdict::Regression
    } else {
        Verdict::NoRegression
    }
}

/// Wilson score 95 % interval of `k` successes in `n`.
pub(crate) fn wilson95(k: u64, n: u64) -> Option<[f64; 2]> {
    if n == 0 || k > n {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "image counts stay far below 2^52"
    )]
    let (k, n) = (k as f64, n as f64);
    let p = k / n;
    let z2 = Z95 * Z95;
    let denom = 1.0 + z2 / n;
    let centre = (p + z2 / (2.0 * n)) / denom;
    let half = Z95 * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / denom;
    Some([(centre - half).max(0.0), (centre + half).min(1.0)])
}

/// A log-scale value as a signed percentage change (`0.0488` → `+5.00`).
pub(crate) fn pct_change(log_ratio: f64) -> f64 {
    log_ratio.exp_m1() * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(x: f64) -> String {
        format!("{x:.6}")
    }

    #[test]
    fn medians_on_fixed_inputs() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[7.0]), Some(7.0));
        assert_eq!(median(&[5.0, 1.0, 3.0]), Some(3.0));
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), Some(2.5));
        // five repetitions, unsorted: the third smallest
        assert_eq!(median(&[12.5, 10.0, 11.0, 30.0, 10.5]), Some(11.0));
    }

    /// Nearest rank in integer arithmetic: 0.95 × 20 must not round to a
    /// rank 18.999… away from 19.
    #[test]
    fn nearest_rank_percentiles_on_fixed_inputs() {
        assert_eq!(nearest_rank(20, 95), 19);
        assert_eq!(nearest_rank(5, 50), 3);
        assert_eq!(nearest_rank(4, 50), 2);
        assert_eq!(nearest_rank(15, 95), 15);
        assert_eq!(nearest_rank(1, 95), 1);
        assert_eq!(nearest_rank(179, 95), 171);
        assert_eq!(nearest_rank(10, 100), 10);
        let values: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(percentile(&values, 50), Some(10.0));
        assert_eq!(percentile(&values, 95), Some(19.0));
        assert_eq!(percentile(&values, 100), Some(20.0));
        assert_eq!(percentile(&[], 50), None);
        assert_eq!(
            spread(&[3.0, 1.0, 2.0]),
            Some(Spread {
                n: 3,
                p50: 2.0,
                p95: 3.0,
                max: 3.0
            })
        );
    }

    /// Exact null distribution, n = 10: 25 of the 1024 sign patterns have
    /// T⁺ ≤ 8, 33 have T⁺ ≤ 9.
    #[test]
    fn signed_rank_distribution_is_exact() {
        let cdf = signed_rank_cdf(10);
        assert_eq!(cdf.len(), 56);
        assert_eq!(&cdf[8..10], &[25.0 / 1024.0, 33.0 / 1024.0]);
        assert_eq!(&cdf[55..], &[1.0]);
        let small = signed_rank_cdf(3);
        let expected = [1.0, 2.0, 3.0, 5.0, 6.0, 7.0, 8.0].map(|c| c / 8.0);
        assert_eq!(small, expected);
    }

    /// k = (two-sided 5 % critical value) + 1 — the standard table values
    /// 0, 2, 3, 5, 8, 25, 52 for n = 6, 7, 8, 9, 10, 15, 20.
    #[test]
    fn signed_rank_critical_ranks_match_the_table() {
        for n in 0..=5 {
            assert_eq!(signed_rank_k(n), None, "n = {n} has no 95 % interval");
        }
        for (n, k) in [(6, 1), (7, 3), (8, 4), (9, 6), (10, 9), (15, 26), (20, 53)] {
            assert_eq!(signed_rank_k(n), Some(k), "n = {n}");
        }
    }

    /// Hand-derived: diffs 1..=10 give 55 Walsh averages; the 9th smallest
    /// is 3.0, the 47th 8.0, the median 5.5.
    #[test]
    fn hodges_lehmann_on_fixed_inputs() {
        let diffs: Vec<f64> = (1..=10).map(f64::from).collect();
        assert_eq!(
            hodges_lehmann(&diffs),
            Some(Shift {
                n: 10,
                estimate: 5.5,
                interval: Some([3.0, 8.0]),
                confidence: Some(1.0 - 50.0 / 1024.0),
            })
        );
        // n = 6: the interval is the full Walsh range at 96.875 % coverage;
        // the 21 averages of these diffs have 2.0 as their 11th value.
        assert_eq!(
            hodges_lehmann(&[-3.0, -1.0, 0.0, 2.0, 5.0, 9.0]),
            Some(Shift {
                n: 6,
                estimate: 2.0,
                interval: Some([-3.0, 9.0]),
                confidence: Some(0.968_75),
            })
        );
        assert_eq!(
            hodges_lehmann(&[1.0, 2.0, 3.0, 4.0, 5.0]),
            Some(Shift {
                n: 5,
                estimate: 3.0,
                interval: None,
                confidence: None,
            }),
            "five pairs cannot reach 95 %"
        );
        assert_eq!(hodges_lehmann(&[]), None);
    }

    #[test]
    fn log_ratios_and_the_symmetric_tail() {
        let pairs = [(100.0, 110.0), (200.0, 200.0), (0.0, 5.0), (50.0, 25.0)];
        let ratios: Vec<String> = log_ratios(&pairs).into_iter().map(fixed).collect();
        assert_eq!(
            ratios,
            [1.1f64.ln(), 0.0, 0.5f64.ln()].map(fixed),
            "a non-positive side carries no ratio"
        );

        // 20 pairs, given shuffled, geometric means 1..=20: the top 10 % is 19, 20.
        let ladder: Vec<(f64, f64)> = (1..=20)
            .map(|i| f64::from((i * 7) % 20 + 1))
            .map(|v| (v, v))
            .collect();
        assert_eq!(tail_pairs(&ladder), vec![(19.0, 19.0), (20.0, 20.0)]);
        // symmetric: (4, 1) and (1, 4) share the mean 2 and rank together
        assert_eq!(
            tail_pairs(&[(4.0, 1.0); 10]).len(),
            tail_pairs(&[(1.0, 4.0); 10]).len()
        );
        // the tail reaches six pairs (a testable interval) at 60 images,
        // so the frozen classes test p95 on zxing 179, gallery 161 and
        // gallery/index-type 61 only
        let tail_of =
            |n: i32| tail_pairs(&(1..=n).map(|i| (f64::from(i), 1.0)).collect::<Vec<_>>()).len();
        assert_eq!(
            (tail_of(59), tail_of(60), tail_of(61), tail_of(179)),
            (5, 6, 6, 17)
        );
        assert_eq!(tail_of(5), 0);
        assert_eq!(nonpositive(&pairs), 1);
    }

    /// The p95 tail: k = ⌊n/10⌋, testable from k ≥ 6 — on both sides of 60
    /// pairs and at the frozen class sizes (gallery 161, zxing 179,
    /// gallery/index-type 61).
    #[test]
    fn the_tail_size_table() {
        let table: Vec<(usize, usize, bool)> = [50, 59, 60, 61, 161, 179]
            .into_iter()
            .map(|n| (n, tail_size(n), tail_size(n) >= MIN_PAIRS))
            .collect();
        assert_eq!(
            table,
            vec![
                (50, 5, false),
                (59, 5, false),
                (60, 6, true),
                (61, 6, true),
                (161, 16, true),
                (179, 17, true),
            ]
        );
        // a tail of 6 pairs has an interval; one of 5 never does
        let ladder = |n: i32| {
            (1..=n)
                .map(|i| (f64::from(i), f64::from(i) * 1.2))
                .collect::<Vec<_>>()
        };
        let tail_shift = |n| hodges_lehmann(&log_ratios(&tail_pairs(&ladder(n))));
        assert_eq!(verdict(tail_shift(59).as_ref(), 0.10), Verdict::NotTestable);
        assert_eq!(verdict(tail_shift(60).as_ref(), 0.10), Verdict::Regression);
    }

    /// The work verdict reads the upper bound against ln(1 − δ).
    #[test]
    fn work_verdicts_need_the_upper_bound_below_minus_delta() {
        let shift = |lower: f64, upper: f64| Shift {
            n: 10,
            estimate: f64::midpoint(lower, upper),
            interval: Some([lower, upper]),
            confidence: Some(0.95),
        };
        // ln 0.95 = −0.0513
        assert_eq!(
            work_verdict(Some(&shift(-0.30, -0.06)), 0.05),
            Verdict::Regression
        );
        assert_eq!(
            work_verdict(Some(&shift(-0.30, -0.04)), 0.05),
            Verdict::NoRegression
        );
        assert_eq!(
            work_verdict(Some(&shift(0.01, 0.30)), 0.05),
            Verdict::NoRegression
        );
        let untestable = Shift {
            interval: None,
            ..shift(-1.0, -0.9)
        };
        assert_eq!(work_verdict(Some(&untestable), 0.05), Verdict::NotTestable);
        assert_eq!(work_verdict(None, 0.05), Verdict::NotTestable);
    }

    /// A deadline checked between equal-cost attempts: an 80 ms budget,
    /// base attempts of 10–80 ms, a candidate 30 % slower per attempt.
    /// Latency reads +2.8 % — no regression — while the work interval lies
    /// far below −5 %: the slower candidate fits less work into the same
    /// budget, which the work verdict of truncated pairs catches.
    #[test]
    fn the_budget_model_hides_from_latency_not_from_work() {
        let run = |cost: f64| {
            let (mut elapsed, mut attempts) = (0.0f64, 0u32);
            while attempts < 40 && elapsed < 80.0 {
                elapsed += cost;
                attempts += 1;
            }
            (elapsed, f64::from(attempts))
        };
        let mut latency = Vec::new();
        let mut work = Vec::new();
        for tenth in (100..=800).step_by(25) {
            let cost = f64::from(tenth) / 10.0;
            let (a_ms, a_n) = run(cost);
            let (b_ms, b_n) = run(cost * 1.3);
            latency.push((a_ms, b_ms));
            work.push((a_n, b_n));
        }
        let pct = |x: f64| format!("{:.3}", pct_change(x));
        let shift = hodges_lehmann(&log_ratios(&latency)).expect("29 images");
        let [lower, upper] = shift.interval.expect("interval");
        assert_eq!(
            (shift.n, pct(shift.estimate), pct(lower), pct(upper)),
            (29, "2.774".into(), "-8.076".into(), "16.276".into())
        );
        assert_eq!(verdict(Some(&shift), 0.05), Verdict::NoRegression);
        let effort = hodges_lehmann(&log_ratios(&work)).expect("29 images");
        let [lower, upper] = effort.interval.expect("interval");
        assert_eq!(
            (pct(effort.estimate), pct(lower), pct(upper)),
            ("-20.943".into(), "-29.289".into(), "-10.557".into())
        );
        assert_eq!(work_verdict(Some(&effort), 0.05), Verdict::Regression);
    }

    /// The `McNemar` test on fixed tables (only candidate, only base):
    /// exact one-sided p = P(X ≥ against), X ~ Bin(n, ½), and the decision
    /// p < 1/20 in integers — five discordant images all against the
    /// candidate are the fewest that can be significant. The 300-image
    /// table crosses the boundary where the tail sum (296 bits) no longer
    /// fits a u128.
    #[test]
    fn mcnemar_on_fixed_tables() {
        for (against, favour, p, significant) in [
            (5, 0, 0.031_25, true),
            (4, 0, 0.0625, false),
            (7, 1, 0.035_156_25, true),
            (6, 1, 0.0625, false),
            (10, 3, 0.046_142_578_125, true),
            (9, 3, 0.072_998_046_875, false),
            (12, 3, 0.017_578_125, true),
            (0, 5, 1.0, false),
            (0, 0, 1.0, false),
        ] {
            let test = mcnemar(against, favour);
            assert_eq!(
                (test.against, test.favour, test.p, test.significant),
                (against, favour, p, significant),
                "{against} against, {favour} favour"
            );
        }
        let below = mcnemar(165, 135);
        let above = mcnemar(164, 136);
        assert_eq!(
            (format!("{:.12}", below.p), below.significant),
            (String::from("0.046951850459"), true)
        );
        assert_eq!(
            (format!("{:.12}", above.p), above.significant),
            (String::from("0.059443168374"), false)
        );
        assert!(
            !mcnemar(0, 300).significant,
            "one-sided: the base's losses never count against the candidate"
        );
    }

    #[test]
    fn verdicts_need_the_lower_bound_above_delta() {
        let shift = |lower: f64| Shift {
            n: 10,
            estimate: lower,
            interval: Some([lower, lower + 0.1]),
            confidence: Some(0.95),
        };
        assert_eq!(
            verdict(Some(&shift(0.06)), 0.05),
            Verdict::Regression,
            "ln 1.05 = 0.0488 < 0.06"
        );
        assert_eq!(verdict(Some(&shift(0.04)), 0.05), Verdict::NoRegression);
        assert_eq!(verdict(Some(&shift(-0.5)), 0.05), Verdict::NoRegression);
        let untestable = Shift {
            interval: None,
            ..shift(1.0)
        };
        assert_eq!(verdict(Some(&untestable), 0.05), Verdict::NotTestable);
        assert_eq!(verdict(None, 0.05), Verdict::NotTestable);
        assert_eq!(size_verdict(1000, 1020, 200), Verdict::NoRegression);
        assert_eq!(size_verdict(1000, 1021, 200), Verdict::Regression);
        assert_eq!(size_verdict(1000, 900, 200), Verdict::NoRegression);
        assert_eq!(size_verdict(0, 5, 200), Verdict::NoRegression);
    }

    /// Reference values: 0/10 → [0, 0.2775], 10/10 → [0.7225, 1],
    /// 5/10 → [0.2366, 0.7634], and the oracle receipt's 169/179 →
    /// 0.900–0.969.
    #[test]
    fn wilson_interval_on_fixed_inputs() {
        let four = |k, n| wilson95(k, n).map(|[l, u]| (format!("{l:.4}"), format!("{u:.4}")));
        let pair = |l: &str, u: &str| Some((l.to_owned(), u.to_owned()));
        assert_eq!(four(0, 10), pair("0.0000", "0.2775"));
        assert_eq!(four(10, 10), pair("0.7225", "1.0000"));
        assert_eq!(four(5, 10), pair("0.2366", "0.7634"));
        let three = wilson95(169, 179).map(|[l, u]| (format!("{l:.3}"), format!("{u:.3}")));
        assert_eq!(three, Some(("0.900".to_owned(), "0.969".to_owned())));
        assert_eq!(wilson95(1, 0), None);
        assert_eq!(wilson95(3, 2), None);
    }

    #[test]
    fn percent_changes() {
        assert_eq!(format!("{:.2}", pct_change(1.05f64.ln())), "5.00");
        assert_eq!(format!("{:.2}", pct_change(0.0)), "0.00");
        assert_eq!(format!("{:.2}", pct_change(0.5f64.ln())), "-50.00");
    }
}
