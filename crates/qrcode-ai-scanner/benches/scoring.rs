//! Scoring-path benchmarks — the FULL profile (decode + full stress score)
//! over real corpus fixtures. The scoring path runs many filtered decode
//! cells per image (probe calibration + 5 ramps + lighting set), so it is
//! the dominant cost of the quality-gate profile and the target of the perf
//! pass.
//!
//! Budget is removed (`budget_ms: None`) so every run executes the WHOLE
//! scoring path to completion — wall-clock cuts would make before/after
//! deltas meaningless (a faster build would simply do more cells before the
//! cut). This is a local wall-clock reference (criterion); CI regression
//! gating runs the corpus-report timings on fixed runners instead.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};
use qrcode_ai_scanner::{ImageInput, ScanConfig, ScanProfile, Scanner};
use std::hint::black_box;

fn fixture(rel: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../fixtures/{rel}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The Full profile with the wall-clock budget removed — the full scoring
/// path always runs to completion (deterministic cost, no budget cut).
fn full_unbudgeted() -> ScanConfig {
    let mut config = ScanProfile::Full.config();
    config.budget_ms = None;
    config
}

fn bench_scoring(c: &mut Criterion) {
    let clean = fixture("clean/gen_v5_q.png");
    let clean_big = fixture("clean/OK_68ms_100_4e875a2c.png");
    let artistic = fixture("artistic/OK_1069ms_85_8b6a54b3.png");
    let monkey = fixture("artistic/blob-style-monkey-logo.webp");

    let full = Scanner::builder()
        .profile(ScanProfile::Custom(full_unbudgeted()))
        .build();

    let mut group = c.benchmark_group("score_full");
    group.sample_size(20);

    group.bench_function("clean_gen_v5_q", |b| {
        b.iter(|| full.scan(black_box(ImageInput::encoded(&clean))).unwrap());
    });
    group.bench_function("clean_ok_68ms", |b| {
        b.iter(|| full.scan(black_box(ImageInput::encoded(&clean_big))).unwrap());
    });
    group.bench_function("artistic_ok_1069ms", |b| {
        b.iter(|| full.scan(black_box(ImageInput::encoded(&artistic))).unwrap());
    });
    group.bench_function("artistic_monkey", |b| {
        b.iter(|| full.scan(black_box(ImageInput::encoded(&monkey))).unwrap());
    });

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default();
    targets = bench_scoring
}
criterion_main!(benches);
