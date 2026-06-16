# Security Policy

`qrcode-ai-scanner` decodes **untrusted images** (uploaded files, camera
frames) across native, Node and browser/WASM targets. Input handling is part
of the threat model, so security reports are taken seriously.

## Supported versions

| Version | Supported |
|---------|-----------|
| 0.3.x   | ✅ |
| < 0.3   | ❌ — 0.2.x was the exploration line; please upgrade |

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

- Preferred: GitHub **private vulnerability reporting** — the *Report a
  vulnerability* button under the repository's *Security* tab.
- Or email **studio.supernovae@gmail.com** with `[security] qrcode-ai-scanner`
  in the subject line.

Please include: the affected version and surface (core / CLI / Node / WASM),
the platform or runtime, a minimal reproducer (the image or bytes if you can
share them), and the observed vs. expected behaviour. We aim to acknowledge
within 72 hours and to ship a fix or mitigation for confirmed issues as a patch
release.

## Hardening already in place

Decoding untrusted input is bounded by design:

- dimension / pixel / engine-side caps and a whole-scan wall-clock budget;
- a decompression-bomb guard and a cap on the number of returned detections;
- per-engine panic isolation — a decoder crash never takes down the scan.

Reports that strengthen these bounds — or find a way around them — are exactly
what we want to hear about.
