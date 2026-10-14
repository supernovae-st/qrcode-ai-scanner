# Corresponding scanner source

This image installs `qrcode-ai-scanner-cli` 0.10.0 using its published Cargo
lockfile, with Rust 1.97.0. The CLI and core remain AGPL-3.0-or-later.

- [Immutable engine source](https://github.com/supernovae-st/qrcode-ai-scanner/tree/2d3b577582979f0dc603b5cb0d96d1be02b459f5)
- [CLI source archive](https://crates.io/api/v1/crates/qrcode-ai-scanner-cli/0.10.0/download), SHA256 `ad253eecca7727805c699f70eb8dbade697fbe4bed71470196d8f85cc321ef7c`
- [Core source archive](https://crates.io/api/v1/crates/qrcode-ai-scanner/0.10.0/download), SHA256 `6feb3f3c27ac1e50775f9752e5524fca81b6319e07aa4b13f8e4136aa2724329`
- [Image build recipe and verification](https://github.com/supernovae-st/qrcode-ai-scanner/tree/main/distribution/docker)

The pinned official Rust/Debian base indexes and image packaging revision are
retained in each build receipt. Releasing an image does not change the engine
source tag or create an HTTP scanner service.
