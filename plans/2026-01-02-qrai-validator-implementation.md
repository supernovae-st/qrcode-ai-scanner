# QRAI-Validator Implementation Plan

**Date**: 2026-01-02
**Methodology**: TDD (Red-Green-Refactor)
**Estimated Tasks**: 8 batches

---

## Batch 1: Project Setup

### Task 1.1: Initialize Workspace
```bash
cd /Users/thibaut/Projects
mv qr-validator qrcode-ai-scanner
cd qrcode-ai-scanner
git init
```

Create `Cargo.toml` (workspace):
```toml
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.2.1"
edition = "2021"
license = "AGPL-3.0"
repository = "https://github.com/supernovae-st/qrcode-ai-scanner"

[workspace.dependencies]
# Decoders
rxing = "0.8"
rqrr = "0.7"

# Image processing
image = "0.25"

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# Error handling
thiserror = "1"
anyhow = "1"

# CLI
clap = { version = "4", features = ["derive"] }

# Testing
pretty_assertions = "1"
```

### Task 1.2: Create qrai-core Crate
```bash
mkdir -p crates/qrai-core/src
```

Create `crates/qrai-core/Cargo.toml`:
```toml
[package]
name = "qrai-core"
version.workspace = true
edition.workspace = true

[dependencies]
rxing.workspace = true
rqrr.workspace = true
image.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true

[dev-dependencies]
pretty_assertions.workspace = true
```

### Task 1.3: Create .gitignore
```
/target
Cargo.lock
*.swp
.DS_Store
node_modules/
*.node
```

**Verification**: `cargo check --workspace`

---

## Batch 2: Core Types (TDD)

### Task 2.1: Write Types Tests First (RED)
File: `crates/qrai-core/src/types.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_result_serializes_to_json() {
        let result = ValidationResult {
            score: 85,
            decodable: true,
            content: Some("https://example.com".to_string()),
            metadata: Some(QrMetadata {
                version: 3,
                error_correction: ErrorCorrectionLevel::H,
                modules: 29,
                decoders_success: vec!["rxing".to_string()],
            }),
            stress_results: StressResults::default(),
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"score\":85"));
        assert!(json.contains("\"decodable\":true"));
    }

    #[test]
    fn stress_results_default_all_false() {
        let sr = StressResults::default();
        assert!(!sr.original);
        assert!(!sr.downscale_50);
    }

    #[test]
    fn error_correction_level_display() {
        assert_eq!(format!("{}", ErrorCorrectionLevel::H), "H");
    }
}
```

### Task 2.2: Implement Types (GREEN)
Make tests pass with minimal implementation.

### Task 2.3: Refactor Types
Clean up, add documentation.

**Verification**: `cargo test -p qrai-core`

---

## Batch 3: Error Types (TDD)

### Task 3.1: Write Error Tests First (RED)
File: `crates/qrai-core/src/error.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_image_load() {
        let err = QraiError::ImageLoad("invalid format".to_string());
        assert!(err.to_string().contains("Failed to load image"));
    }

    #[test]
    fn error_display_decode_failed() {
        let err = QraiError::DecodeFailed;
        assert!(err.to_string().contains("No QR code"));
    }
}
```

### Task 3.2: Implement Errors (GREEN)
```rust
#[derive(Debug, thiserror::Error)]
pub enum QraiError {
    #[error("Failed to load image: {0}")]
    ImageLoad(String),

    #[error("No QR code found in image")]
    DecodeFailed,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

**Verification**: `cargo test -p qrai-core`

---

## Batch 4: Decoder Module (TDD)

### Task 4.1: Write Decoder Tests First (RED)
File: `crates/qrai-core/src/decoder.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Need a real QR code image for this test
    // We'll create one programmatically or use test-images/

    #[test]
    fn decode_simple_qr_with_rxing() {
        let qr_bytes = include_bytes!("../../test-images/clean/simple.png");
        let result = decode_with_rxing(qr_bytes);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().content, "https://example.com");
    }

    #[test]
    fn decode_simple_qr_with_rqrr() {
        let qr_bytes = include_bytes!("../../test-images/clean/simple.png");
        let result = decode_with_rqrr(qr_bytes);
        assert!(result.is_ok());
    }

    #[test]
    fn multi_decode_tries_both() {
        let qr_bytes = include_bytes!("../../test-images/clean/simple.png");
        let result = multi_decode(qr_bytes).unwrap();
        // Should have tried both decoders
        assert!(!result.decoders_success.is_empty());
    }

    #[test]
    fn decode_invalid_image_returns_error() {
        let garbage = b"not an image";
        let result = multi_decode(garbage);
        assert!(result.is_err());
    }
}
```

### Task 4.2: Generate Test QR Image
Before implementing, create a simple QR code for testing:

```rust
// In a build script or setup
fn generate_test_qr() {
    use qrcode::QrCode;
    use image::Luma;

    let code = QrCode::new(b"https://example.com").unwrap();
    let img = code.render::<Luma<u8>>().build();
    img.save("test-images/clean/simple.png").unwrap();
}
```

### Task 4.3: Implement decode_with_rxing (GREEN)
```rust
pub fn decode_with_rxing(image_bytes: &[u8]) -> Result<DecodeResult, QraiError> {
    let img = image::load_from_memory(image_bytes)
        .map_err(|e| QraiError::ImageLoad(e.to_string()))?;

    let results = rxing::helpers::detect_multiple_in_luma(img.to_luma8())
        .map_err(|_| QraiError::DecodeFailed)?;

    let first = results.first().ok_or(QraiError::DecodeFailed)?;

    Ok(DecodeResult {
        content: first.getText().to_string(),
        decoder: "rxing".to_string(),
        // Extract metadata from result...
    })
}
```

### Task 4.4: Implement decode_with_rqrr (GREEN)
```rust
pub fn decode_with_rqrr(image_bytes: &[u8]) -> Result<DecodeResult, QraiError> {
    let img = image::load_from_memory(image_bytes)
        .map_err(|e| QraiError::ImageLoad(e.to_string()))?;

    let gray = img.to_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(gray);
    let grids = prepared.detect_grids();

    let grid = grids.first().ok_or(QraiError::DecodeFailed)?;
    let (_meta, content) = grid.decode().map_err(|_| QraiError::DecodeFailed)?;

    Ok(DecodeResult {
        content,
        decoder: "rqrr".to_string(),
    })
}
```

### Task 4.5: Implement multi_decode (GREEN)
```rust
pub fn multi_decode(image_bytes: &[u8]) -> Result<MultiDecodeResult, QraiError> {
    let mut decoders_success = Vec::new();
    let mut content = None;
    let mut metadata = None;

    // Try rxing first (more robust)
    if let Ok(result) = decode_with_rxing(image_bytes) {
        decoders_success.push("rxing".to_string());
        content = Some(result.content.clone());
        metadata = result.metadata;
    }

    // Try rqrr as well (for scoring bonus)
    if let Ok(result) = decode_with_rqrr(image_bytes) {
        decoders_success.push("rqrr".to_string());
        if content.is_none() {
            content = Some(result.content);
        }
    }

    if decoders_success.is_empty() {
        return Err(QraiError::DecodeFailed);
    }

    Ok(MultiDecodeResult {
        content: content.unwrap(),
        metadata,
        decoders_success,
    })
}
```

**Verification**: `cargo test -p qrai-core decoder`

---

## Batch 5: Scorer Module (TDD)

### Task 5.1: Write Scorer Tests First (RED)
File: `crates/qrai-core/src/scorer.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_all_pass_is_100() {
        let stress = StressResults {
            original: true,
            downscale_50: true,
            downscale_25: true,
            blur_light: true,
            blur_medium: true,
            low_contrast: true,
        };
        let score = calculate_score(&stress, 2); // both decoders
        assert_eq!(score, 100);
    }

    #[test]
    fn score_only_original_is_low() {
        let stress = StressResults {
            original: true,
            downscale_50: false,
            downscale_25: false,
            blur_light: false,
            blur_medium: false,
            low_contrast: false,
        };
        let score = calculate_score(&stress, 1);
        assert!(score < 30); // Only original passed
    }

    #[test]
    fn stress_test_downscale_50() {
        let qr_bytes = include_bytes!("../../test-images/clean/simple.png");
        let result = run_stress_test_downscale_50(qr_bytes);
        assert!(result); // Clean QR should pass
    }
}
```

### Task 5.2: Implement Image Transforms
```rust
fn downscale(img: &DynamicImage, factor: f32) -> DynamicImage {
    let (w, h) = img.dimensions();
    let new_w = (w as f32 * factor) as u32;
    let new_h = (h as f32 * factor) as u32;
    img.resize(new_w, new_h, image::imageops::FilterType::Lanczos3)
}

fn apply_blur(img: &DynamicImage, sigma: f32) -> DynamicImage {
    img.blur(sigma)
}

fn reduce_contrast(img: &DynamicImage, factor: f32) -> DynamicImage {
    img.adjust_contrast(factor * -50.0)
}
```

### Task 5.3: Implement Stress Tests
```rust
pub fn run_stress_tests(image_bytes: &[u8]) -> Result<StressResults, QraiError> {
    let img = image::load_from_memory(image_bytes)
        .map_err(|e| QraiError::ImageLoad(e.to_string()))?;

    Ok(StressResults {
        original: multi_decode(image_bytes).is_ok(),
        downscale_50: test_variant(&downscale(&img, 0.5)),
        downscale_25: test_variant(&downscale(&img, 0.25)),
        blur_light: test_variant(&apply_blur(&img, 1.0)),
        blur_medium: test_variant(&apply_blur(&img, 2.0)),
        low_contrast: test_variant(&reduce_contrast(&img, 0.5)),
    })
}

fn test_variant(img: &DynamicImage) -> bool {
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png).ok();
    multi_decode(&buf).is_ok()
}
```

### Task 5.4: Implement Score Calculation
```rust
pub fn calculate_score(stress: &StressResults, num_decoders: usize) -> u8 {
    let mut score = 0u32;
    let mut total = 100u32;

    // Weights
    if stress.original { score += 20; }
    if stress.downscale_50 { score += 15; }
    if stress.downscale_25 { score += 10; }
    if stress.blur_light { score += 15; }
    if stress.blur_medium { score += 10; }
    if stress.low_contrast { score += 15; }

    // Bonus for multiple decoders
    if num_decoders >= 2 { score += 15; }

    ((score * 100) / total).min(100) as u8
}
```

**Verification**: `cargo test -p qrai-core scorer`

---

## Batch 6: Public API (TDD)

### Task 6.1: Write API Tests First (RED)
File: `crates/qrai-core/src/lib.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_returns_full_result() {
        let qr_bytes = include_bytes!("../test-images/clean/simple.png");
        let result = validate(qr_bytes).unwrap();

        assert!(result.decodable);
        assert!(result.score > 0);
        assert!(result.content.is_some());
        assert!(result.metadata.is_some());
    }

    #[test]
    fn validate_garbage_returns_error() {
        let result = validate(b"not an image");
        assert!(result.is_err());
    }

    #[test]
    fn decode_only_is_fast() {
        let qr_bytes = include_bytes!("../test-images/clean/simple.png");

        let start = std::time::Instant::now();
        let _ = decode_only(qr_bytes);
        let elapsed = start.elapsed();

        assert!(elapsed.as_millis() < 100); // Should be fast
    }
}
```

### Task 6.2: Implement Public API (GREEN)
```rust
pub fn validate(image_bytes: &[u8]) -> Result<ValidationResult, QraiError> {
    let decode_result = decoder::multi_decode(image_bytes)?;
    let stress_results = scorer::run_stress_tests(image_bytes)?;
    let score = scorer::calculate_score(
        &stress_results,
        decode_result.decoders_success.len()
    );

    Ok(ValidationResult {
        score,
        decodable: true,
        content: Some(decode_result.content),
        metadata: Some(QrMetadata {
            version: decode_result.metadata.map(|m| m.version).unwrap_or(0),
            error_correction: decode_result.metadata
                .map(|m| m.error_correction)
                .unwrap_or(ErrorCorrectionLevel::M),
            modules: decode_result.metadata.map(|m| m.modules).unwrap_or(0),
            decoders_success: decode_result.decoders_success,
        }),
        stress_results,
    })
}

pub fn decode_only(image_bytes: &[u8]) -> Result<DecodeResult, QraiError> {
    decoder::multi_decode(image_bytes).map(|r| DecodeResult {
        content: r.content,
        metadata: r.metadata,
    })
}
```

**Verification**: `cargo test -p qrai-core`

---

## Batch 7: CLI Binary

### Task 7.1: Create CLI Crate
File: `crates/qrcode-ai-scanner-cli/Cargo.toml`
```toml
[package]
name = "qrcode-ai-scanner-cli"
version.workspace = true
edition.workspace = true

[[bin]]
name = "qrcode-ai-scanner"
path = "src/main.rs"

[dependencies]
qrai-core = { path = "../qrai-core" }
clap.workspace = true
serde_json.workspace = true
anyhow.workspace = true
```

### Task 7.2: Implement CLI
File: `crates/qrcode-ai-scanner-cli/src/main.rs`
```rust
use clap::Parser;
use qrai_core::{validate, decode_only};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "qrcode-ai-scanner")]
#[command(about = "Validate QR codes and compute scannability score")]
struct Cli {
    /// Image file to validate
    image: PathBuf,

    /// Output only the score (0-100)
    #[arg(long)]
    score_only: bool,

    /// Fast mode: decode only, no stress tests
    #[arg(long)]
    decode_only: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let image_bytes = std::fs::read(&cli.image)?;

    if cli.decode_only {
        let result = decode_only(&image_bytes)?;
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if cli.score_only {
        let result = validate(&image_bytes)?;
        println!("{}", result.score);
    } else {
        let result = validate(&image_bytes)?;
        println!("{}", serde_json::to_string_pretty(&result)?);
    }

    Ok(())
}
```

**Verification**:
```bash
cargo build -p qrcode-ai-scanner-cli
./target/debug/qrcode-ai-scanner test-images/clean/simple.png
```

---

## Batch 8: Node.js Binding

### Task 8.1: Create Node Crate
File: `crates/qrai-node/Cargo.toml`
```toml
[package]
name = "qrai-node"
version.workspace = true
edition.workspace = true

[lib]
crate-type = ["cdylib"]

[dependencies]
qrai-core = { path = "../qrai-core" }
napi = "2"
napi-derive = "2"

[build-dependencies]
napi-build = "2"
```

### Task 8.2: Create package.json
```json
{
  "name": "@supernovae-st/qrcode-ai-scanner",
  "version": "0.1.0",
  "main": "index.js",
  "types": "index.d.ts",
  "napi": {
    "name": "qrcode-ai-scanner",
    "triples": {
      "defaults": true,
      "additional": ["aarch64-apple-darwin"]
    }
  },
  "devDependencies": {
    "@napi-rs/cli": "^2.18.0"
  },
  "scripts": {
    "build": "napi build --platform --release",
    "prepublishOnly": "napi prepublish -t npm"
  }
}
```

### Task 8.3: Implement Node Binding
File: `crates/qrai-node/src/lib.rs`
```rust
use napi::bindgen_prelude::*;
use napi_derive::napi;
use qrai_core::{ValidationResult as CoreResult, validate as core_validate};

#[napi(object)]
pub struct ValidationResult {
    pub score: u8,
    pub decodable: bool,
    pub content: Option<String>,
    // Flatten metadata and stress_results for JS convenience
}

#[napi]
pub fn validate(image_buffer: Buffer) -> Result<ValidationResult> {
    let result = core_validate(&image_buffer)
        .map_err(|e| Error::from_reason(e.to_string()))?;

    Ok(ValidationResult {
        score: result.score,
        decodable: result.decodable,
        content: result.content,
    })
}

#[napi]
pub fn validate_score_only(image_buffer: Buffer) -> Result<u8> {
    let result = core_validate(&image_buffer)
        .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(result.score)
}
```

**Verification**:
```bash
cd crates/qrai-node
npm install
npm run build
node -e "const m = require('./'); console.log(m)"
```

---

## Test Images Setup

Create test QR codes:
```bash
mkdir -p test-images/{clean,artistic,degraded}
```

Generate with qrcode crate or download samples.

---

## Final Verification Checklist

- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace` no warnings
- [ ] `cargo fmt --check` passes
- [ ] CLI works: `qrcode-ai-scanner test.png`
- [ ] Node binding works: `node -e "require('./').validate(...)"`
- [ ] Performance: <200ms on standard QR
