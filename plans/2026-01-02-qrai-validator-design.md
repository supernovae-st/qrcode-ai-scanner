# QRAI-Validator Design Document

**Date**: 2026-01-02
**Status**: Approved
**Author**: Thibaut MÉLEN

---

## Overview

QRAI-Validator is a high-performance Rust library and CLI tool for validating QR codes and computing a "scannability score". It's designed to integrate with QR Code AI SaaS platform via napi-rs Node.js bindings.

## Problem Statement

QR Code AI generates artistic QR codes with:
- Custom colors and gradients
- Logo overlays
- AI-generated artistic styles (shadows/highlights forming the QR pattern)
- Deformed module shapes

Users need real-time feedback (<200ms) on whether their customized QR code is still scannable.

## Requirements

### Functional
- Decode QR codes from PNG/JPEG image buffers
- Return decoded content and QR metadata (version, error correction level, modules)
- Compute scannability score 0-100
- Support artistic/stylized QR codes

### Non-Functional
- Latency: <200ms target, <50ms ideal
- Pure Rust (no system dependencies for easy deployment)
- Node.js integration via napi-rs
- Thread-safe for concurrent requests

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        qrcode-ai-scanner                           │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐             │
│  │  qrcode-ai-scanner-cli   │  │ qrai-node   │  │   (future)  │             │
│  │   binary    │  │  napi-rs    │  │    wasm     │             │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘             │
│         │                │                │                     │
│         └────────────────┼────────────────┘                     │
│                          │                                      │
│                   ┌──────▼──────┐                               │
│                   │  qrai-core  │                               │
│                   │    (lib)    │                               │
│                   └──────┬──────┘                               │
│                          │                                      │
│         ┌────────────────┼────────────────┐                     │
│         │                │                │                     │
│  ┌──────▼──────┐  ┌──────▼──────┐  ┌──────▼──────┐             │
│  │   decoder   │  │   scorer    │  │ preprocess  │             │
│  │ rxing+rqrr  │  │stress tests │  │  (future)   │             │
│  └─────────────┘  └─────────────┘  └─────────────┘             │
└─────────────────────────────────────────────────────────────────┘
```

## Core Types

```rust
#[derive(Debug, Clone, Serialize)]
pub struct ValidationResult {
    pub score: u8,                      // 0-100
    pub decodable: bool,
    pub content: Option<String>,
    pub metadata: Option<QrMetadata>,
    pub stress_results: StressResults,
}

#[derive(Debug, Clone, Serialize)]
pub struct QrMetadata {
    pub version: u8,                    // 1-40
    pub error_correction: ErrorCorrectionLevel,
    pub modules: u8,                    // 21, 25, 29...
    pub decoders_success: Vec<String>,  // ["rxing", "rqrr"]
}

#[derive(Debug, Clone, Serialize)]
pub struct StressResults {
    pub original: bool,
    pub downscale_50: bool,
    pub downscale_25: bool,
    pub blur_light: bool,
    pub blur_medium: bool,
    pub low_contrast: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum ErrorCorrectionLevel {
    L, // ~7% recovery
    M, // ~15% recovery
    Q, // ~25% recovery
    H, // ~30% recovery
}
```

## Decoding Strategy

### Multi-Decoder Cascade

1. **Primary**: `rxing` (ZXing-Cpp powered, most robust)
2. **Fallback**: `rqrr` (pure Rust, fast, good for clean QR)

If primary succeeds, we still try fallback to populate `decoders_success` for scoring bonus.

### Stress Tests for Scoring

| Test | Transform | Weight |
|------|-----------|--------|
| original | none | 20 |
| downscale_50 | resize 50% | 15 |
| downscale_25 | resize 25% | 10 |
| blur_light | gaussian σ=1.0 | 15 |
| blur_medium | gaussian σ=2.0 | 10 |
| low_contrast | reduce 50% | 15 |
| **bonus**: both decoders | - | 15 |

**Score formula**:
```
score = sum(passed_test * weight) / total_weight * 100
```

## API Design

### Rust Library

```rust
// Main entry point
pub fn validate(image_bytes: &[u8]) -> Result<ValidationResult, QraiError>;

// Convenience for CLI
pub fn validate_from_path(path: &Path) -> Result<ValidationResult, QraiError>;

// Fast mode (no stress tests, just decode)
pub fn decode_only(image_bytes: &[u8]) -> Result<DecodeResult, QraiError>;
```

### Node.js (napi-rs)

```typescript
import { validate } from '@supernovae-st/qrcode-ai-scanner';

const result = await validate(imageBuffer);
// {
//   score: 85,
//   decodable: true,
//   content: "https://example.com",
//   metadata: { version: 3, errorCorrection: "H", modules: 29 },
//   stressResults: { original: true, downscale50: true, ... }
// }
```

### CLI

```bash
# Full validation with JSON output
qrcode-ai-scanner image.png

# Score only (for scripts)
qrcode-ai-scanner --score-only image.png
# Output: 85

# Decode only (fast, no stress tests)
qrcode-ai-scanner --decode-only image.png
```

## Dependencies

```toml
[dependencies]
rxing = "0.8"          # Primary decoder (ZXing port)
rqrr = "0.7"           # Fallback decoder (Quirc port)
image = "0.25"         # Image loading and transforms
thiserror = "1"        # Error handling
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# CLI only
clap = { version = "4", features = ["derive"] }

# Node binding only
napi = "2"
napi-derive = "2"
```

## Project Structure

```
qrcode-ai-scanner/
├── Cargo.toml              (workspace)
├── .gitignore
├── README.md
├── docs/
│   └── plans/
│       └── 2026-01-02-qrcode-ai-scanner-design.md
├── crates/
│   ├── qrai-core/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── types.rs
│   │       ├── decoder.rs
│   │       ├── scorer.rs
│   │       ├── error.rs
│   │       └── preprocessing.rs
│   ├── qrcode-ai-scanner-cli/
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   └── qrai-node/
│       ├── Cargo.toml
│       ├── package.json
│       └── src/lib.rs
└── test-images/
    ├── clean/
    ├── artistic/
    └── degraded/
```

## Implementation Plan

See: `docs/plans/2026-01-02-qrcode-ai-scanner-implementation.md`

## Future Enhancements

- **v1.1**: Preprocessing pipeline (contrast enhancement, denoise)
- **v1.2**: Diagnostic recommendations ("increase contrast", "reduce logo size")
- **v2.0**: WASM build for browser-side validation
- **v2.1**: OpenCV integration for extremely artistic QR codes
