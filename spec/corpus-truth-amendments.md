# Corpus truth amendments

An amendment changes the text used by corpus instruments without replacing
upstream images or truth files. The original files and their hashes remain
the integrity reference. This does not change scanner acceptance, scoring,
content identity, or the library's wire contract.

The strict oracle, external capability tripwire and rotation dashboard use
one shared resolver. It binds an amendment to the original image path and
SHA-256, the original truth SHA-256 and bytes, and the effective text SHA-256.
A mismatch or missing amended truth is an error, including when the scanner
returns no detections. Rotation validates the original encoded image before
transforming its pixels. Unrelated truths retain the original decoding rule:
UTF-8, otherwise ISO-8859-1, without trimming.

Oracle receipts identify these judge rules as `qrscan-oracle/v2.1`, retain
`truth_sha256` as the original source-file hash and
add `truth_amendment` with effective-text and evidence provenance. The input
`truth_amendments_sha256` fingerprints the immutable amendment record. The
complete detection list is still judged normally against effective text:
extra or wrong output blocks. A historical `known_wrong` disposition becomes
`superseded` only when this exact amendment validates; it does not excuse any
output. Status pins are updated from subsequent measurements, independently
of the unchanged upstream file hashes.

## zxing-qrcode-2-13

Amendment ID: `zxing-qrcode-2-13-text-2026-10-13`.

| Input | SHA-256 |
|---|---|
| `zxing-blackbox/qrcode-2/13.png` (original 256 × 256 image) | `a8d498d6d2d6a3e27e24fd75ed023cd5b8b6b42cb4996674e88e555d68855ec1` |
| `zxing-blackbox/qrcode-2/13.txt` (original 71-byte text) | `c19d2dcb13469aae263a41cd73add68c45e086223a04c41368128c47143c0e5a` |
| Effective 72-byte ASCII text | `30a2a450189a5b357a360265102c4c99baa4c84a2559b6a9b16d9a0d5bf80a07` |

Original text: `The 2005 USGS aerial photograph of the Washington Monument is censored.`

Effective text: `The 2005 USGS aerial photography of the Washington Monument is censored.`

Two external decoder families confirmed the effective text on pixels derived
from the exact original image. Neither was given the desired payload; the
scanner under test was not used as a truth source. Multiple related ZXing
implementations alone would not establish this confirmation.

| Reader | Artifact SHA-256 | Archived execution receipt SHA-256 |
|---|---|---|
| ZXing-C++ 3.1.1 (native Python extension) | `1f0f117c6bc35092f9e1cd42d6d6fb0ed7f0fcf52faebecf996713a8e6179f6b` | `1b2b757849106c24b2c76c364eabe1eafced4bc39e781f834769ef18bb8e8635` |
| External C quirc via `quirc-wasm-emcc` 1.0.26 | `5711597fef34ffe13f87c3252fe43d578447b465bbe60941e010d8651aa1422f` | `657851092cc4182601bac8dfdf643d8dc4b1073d0f3396d317f47d8e1832e375` |

ZXing-C++ returned the effective text in one fresh invocation after adding a
32-pixel white border and scaling by 2 with nearest-neighbor interpolation,
using its GlobalHistogram binarizer. External quirc returned the same ASCII
text in 13 of 20 executed original-derived variants; seven returned no text,
including the untransformed original, and none failed initialization. The
successful quirc variants used grayscale, contrast and Gaussian blur, with
some also using borders and resizing. These are observations of transformed
original pixels, not a claim that either reader decodes the original unaided.

The paired execution receipt is pinned by SHA-256
`a5386ff61e9be111183067f9ae5c301dc3ad229a9721b39d86dc5aa29a9c3680`.
The quirc reporting correction, distinguishing sampled RSS from worker-end
RSS reports and retaining all seven no-reads, is pinned by
`d4ce9b94e0ebe97110e8ebb970f75f373335a94a69aad75223c1d99000125e81`.
The exact quirc package passed npm integrity verification; attribution to C
quirc follows that package's documentation and associated C source. A
reproducible source-to-WASM build was **not** established. rqrr shares quirc
ancestry, but these external C/WASM observations ran independently of the
scanner and are paired with the separate ZXing family.

This amendment qualifies **text only**. It establishes neither ECI/FNC1
declarations nor the native QR payload length through the quirc wrapper.
No structural `raw_hex`, ECI or FNC1 truth is inferred from it. The nested
symbol in `qrcode-2/16.png` remains under its existing incomplete-truth
disposition; no other corpus row is amended.
