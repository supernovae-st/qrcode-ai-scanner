# Fixtures

Ground truth lives in [`../corpus.toml`](../corpus.toml) — every image here
has an entry (path · category · expected text or negative).

- `clean/` — legacy v0.2 reference + the generated version×EC matrix
  (`cargo run -p xtask -- gen-fixtures`, deterministic, committed once).
- `artistic/` — AI-generated artistic QR codes (the specialty). Grows with
  the qrcode-ai.com generator dump.
- `degraded/` — deterministic degradations of clean cells + real failures.

`cargo run -p xtask -- corpus-report` runs the scanner over everything and
fails on any regression; `--write` refreshes the README results block.
