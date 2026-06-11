//! Decode + score benchmarks over the committed corpus images.
//! Local wall-clock reference (criterion); CI regression gating runs the
//! corpus-report timings on fixed runners instead — see ci.yml.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};
use qrcode_ai_scanner::{ImageInput, ScanProfile, Scanner};
use std::hint::black_box;

fn fixture(rel: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/../../fixtures/{rel}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn bench_decode(c: &mut Criterion) {
    let clean = fixture("clean/gen_v5_q.png");
    let artistic = fixture("artistic/OK_1069ms_85_8b6a54b3.png");

    let frame = Scanner::builder().profile(ScanProfile::Frame).build();
    let fast = Scanner::builder().profile(ScanProfile::Fast).build();
    let full = Scanner::builder().profile(ScanProfile::Full).build();

    c.bench_function("frame_clean", |b| {
        b.iter(|| frame.scan(black_box(ImageInput::encoded(&clean))).unwrap());
    });
    c.bench_function("fast_clean_scored", |b| {
        b.iter(|| fast.scan(black_box(ImageInput::encoded(&clean))).unwrap());
    });
    c.bench_function("full_artistic", |b| {
        b.iter(|| {
            full.scan(black_box(ImageInput::encoded(&artistic)))
                .unwrap()
        });
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = bench_decode
}
criterion_main!(benches);
