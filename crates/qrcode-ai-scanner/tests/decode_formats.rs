//! Format coverage on the decode side. The scanner guesses the format
//! from the bytes (`ImageReader::with_guessed_format`), so it decodes
//! any container the `image` feature set enables. JPEG is already pinned
//! by the EXIF phone-photo fixtures; these vectors prove the other
//! enabled decoders (PNG, WebP, GIF) actually reach the ladder, and that
//! a QR round-trips through each back to the same text.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use qrcode_ai_scanner::{ImageInput, Scanner};

const CONTENT: &str = "https://qrcode-ai.com/decode-formats";

fn qr_as(fmt: image::ImageFormat, module_px: u32) -> Vec<u8> {
    let code = qrcode::QrCode::with_error_correction_level(CONTENT, qrcode::EcLevel::Q).unwrap();
    let img = code
        .render::<image::Luma<u8>>()
        .module_dimensions(module_px, module_px)
        .build();
    // encode from RGBA: the GIF encoder rejects Luma8 (palette formats
    // want colour input), and RGBA is a harmless superset for the rest
    let rgba = image::DynamicImage::ImageLuma8(img).to_rgba8();
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut std::io::Cursor::new(&mut buf), fmt)
        .unwrap();
    buf
}

#[test]
fn a_qr_scans_back_from_every_lossless_container() {
    for (name, fmt) in [
        ("png", image::ImageFormat::Png),
        ("webp", image::ImageFormat::WebP),
        ("gif", image::ImageFormat::Gif),
    ] {
        let bytes = qr_as(fmt, 8);
        let report = Scanner::default()
            .scan(ImageInput::encoded(&bytes))
            .unwrap_or_else(|e| panic!("{name}: scan errored: {e}"));
        assert!(!report.detections.is_empty(), "{name}: nothing decoded");
        assert_eq!(
            report.detections[0].content.text, CONTENT,
            "{name}: wrong text"
        );
    }
}

#[test]
fn a_qr_scans_back_from_a_synthetic_jpeg_at_print_module_size() {
    // JPEG is lossy: give each module enough pixels that DCT ringing
    // stays clear of the module centers (a phone photo has the same slack)
    let bytes = qr_as(image::ImageFormat::Jpeg, 12);
    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .expect("jpeg scan");
    assert_eq!(report.detections[0].content.text, CONTENT);
}
