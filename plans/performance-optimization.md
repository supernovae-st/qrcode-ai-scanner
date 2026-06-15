# QRAI Scanner - Performance Optimization Plan

## Current State

| QR Type | Current Time | Target Time |
|---------|--------------|-------------|
| Simple/Clean | 80-300ms | <50ms |
| Artistic (complex) | 5-11 SECONDS | <500ms |

## Root Causes Identified

### 1. Strategy 24: Random Preprocessing (~2000ms)
- **Lines 282-283**: 50 tries x 2 decoders = 100+ decode attempts
- Each try: `img.clone()` (750KB) + blur + contrast + brightness
- **Action: REMOVE entirely**

### 2. Double to_luma8() Conversion (~100ms waste)
- **Lines 25, 54**: Both decoders convert to luma8
- Called via `try_decode_with_both()` = 2x conversions per attempt
- **Action: Pre-convert once, pass reference**

### 3. No Early Exit on Decoder Success
- **Lines 290-339**: Always tries both decoders even if first succeeds
- 24 strategies x 2 decoders = 48 minimum attempts
- **Action: Return immediately if rxing finds content**

### 4. Slow Strategies Too Early
- Adaptive threshold (line 114): O(w*h*block^2) = ~150ms
- Median filter (line 266): 250K sorts = ~200ms
- Morphology (line 202): 4 image passes = ~100ms
- Saturation aggressive (line 261): 36 combos = ~500ms
- **Action: Reorder + remove slowest**

### 5. Sequential Strategy Execution
- **Lines 84-287**: 24 strategies run sequentially
- Channel extractions, thresholds could parallelize
- **Action: Use rayon par_iter with find_map_first**

## Implementation Plan

### Phase 1: Quick Wins (Expected: 5s -> 1s)

#### 1.1 Remove Random Preprocessing
```rust
// DELETE lines 280-284:
// Strategy 24: Brute-force random preprocessing
// if let Ok(result) = try_random_preprocessing(img, 50) { ... }
```

#### 1.2 Remove Median Filter
```rust
// DELETE lines 266-273:
// if let Ok(result) = try_with_median_filter(img) { ... }
```

#### 1.3 Early Exit in try_decode_with_both
```rust
// Line 297-306: If rxing succeeds with content, return immediately
if let Ok(result) = decode_with_rxing(img) {
    return Ok(MultiDecodeResult {
        content: result.content,
        metadata: Some(QrMetadata { ... }),
        decoders_success: vec!["rxing".to_string()],
    });
}
// Only try rqrr if rxing failed
```

### Phase 2: Memory Optimization (Expected: 1s -> 600ms)

#### 2.1 Single luma8 Conversion
```rust
fn try_decode_with_both(img: &DynamicImage) -> Result<MultiDecodeResult> {
    let luma = img.to_luma8();  // Convert ONCE
    let (width, height) = luma.dimensions();

    if let Ok(result) = decode_with_rxing_luma(&luma, width, height) {
        return Ok(result);
    }
    decode_with_rqrr_luma(&luma, width, height)
}
```

#### 2.2 Update Decoder Signatures
```rust
pub fn decode_with_rxing_luma(luma: &GrayImage, width: u32, height: u32) -> Result<SingleDecodeResult>
pub fn decode_with_rqrr_luma(luma: &GrayImage, width: u32, height: u32) -> Result<SingleDecodeResult>
```

### Phase 3: Strategy Reordering (Expected: 600ms -> 400ms)

New order by speed tier:

```
FAST TIER (cumulative ~100ms):
1. Original
2. Enhanced contrast
3. Otsu threshold
4. Invert
5. High contrast threshold
6. Fixed thresholds (5x)
7. Inverted Otsu

MODERATE TIER (~150ms):
8. Sharpen + threshold
9. Color channels (R,G,B,Sat)
10. HSV Hue/Value
11. Custom grayscale (4x)
12. (R+B)/2 - G
13. Green inverted

SLOW TIER (~200ms, only if needed):
14. Saturation aggressive (moved from 21)
15. Downsample + process
16. Color distance

END-GAME TIER (~100ms, last resort):
17. Edge detection
18. Adaptive threshold

REMOVED:
- Random preprocessing (was 24)
- Median filter (was 22)
- Morphology (was 15)
- Saturation morph (was 23)
```

### Phase 4: Parallelization (Expected: 400ms -> 250ms)

#### 4.1 Parallel Channel Processing
```rust
use rayon::prelude::*;

// Parallelize color channel attempts
let channels = ["red", "green", "blue", "saturation"];
if let Some(result) = channels.par_iter()
    .find_map_first(|channel| {
        let extracted = extract_channel(img, channel);
        try_decode_with_both(&extracted).ok()
    }) {
    return Ok(result);
}
```

#### 4.2 Parallel Custom Grayscale
```rust
let weights = [(0.1, 0.1, 0.8), (0.0, 0.0, 1.0), (0.5, 0.0, 0.5), (0.33, 0.33, 0.34)];
if let Some(result) = weights.par_iter()
    .find_map_first(|&(r, g, b)| {
        let gray = custom_grayscale(img, r, g, b);
        try_decode_with_both(&gray).ok()
    }) {
    return Ok(result);
}
```

### Phase 5: Advanced Optimizations (Bonus)

#### 5.1 SmallVec for Decoder List
```toml
# Cargo.toml
smallvec = "1.11"
```
```rust
use smallvec::SmallVec;
type DecoderList = SmallVec<[String; 2]>;  // No heap for 1-2 items
```

#### 5.2 FxHashMap for Caching
```toml
rustc-hash = "1.1"
```
```rust
use rustc_hash::FxHashMap;
// Cache preprocessing results to avoid redundant work
```

#### 5.3 Selection Algorithm for Extreme Contrast
```rust
// Replace full sort with O(n) selection
fn apply_extreme_contrast(img: &DynamicImage) -> DynamicImage {
    let mut values: Vec<u8> = gray.pixels().map(|p| p.0[0]).collect();
    let low_idx = values.len() * 5 / 100;
    let high_idx = values.len() * 95 / 100;

    // O(n) selection instead of O(n log n) sort
    let low = *values.select_nth_unstable(low_idx).1;
    let high = *values.select_nth_unstable(high_idx).1;
    // ...
}
```

## Dependencies to Add

```toml
[workspace.dependencies]
smallvec = "1.11"
rustc-hash = "1.1"
```

## Verification

After each phase, run:
```bash
cargo build -p qrcode-ai-scanner-cli --release
time ./target/release/qrcode-ai examples/qrcodes/qrcode-ai-*.png
```

Expected results:
- Phase 1: 5s -> 1s (80% reduction)
- Phase 2: 1s -> 600ms (40% reduction)
- Phase 3: 600ms -> 400ms (33% reduction)
- Phase 4: 400ms -> 250ms (37% reduction)
- Phase 5: 250ms -> <200ms (target achieved)

## Files to Modify

1. `crates/qrai-core/src/decoder.rs` - Main changes
2. `crates/qrai-core/Cargo.toml` - Add smallvec, rustc-hash
3. `Cargo.toml` - Workspace deps

## Execution Order

1. Remove random preprocessing + median filter (5 min)
2. Add early exit in try_decode_with_both (10 min)
3. Single luma8 conversion (15 min)
4. Strategy reordering (20 min)
5. Add rayon parallelization (30 min)
6. Optional: SmallVec, FxHashMap, selection algo (20 min)

**Total estimated time: 1.5-2 hours**
