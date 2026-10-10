# Fixtures

Ground truth of the corpus lives in [`../corpus.toml`](../corpus.toml) —
every corpus image has an entry (path · category · expected text or
negative).

- `clean/` — legacy v0.2 reference + the generated version×EC matrix
  (`cargo run -p xtask -- gen-fixtures`, deterministic, committed once).
- `artistic/` — AI-generated artistic QR codes (the specialty). Grows with
  the qrcode-ai.com generator dump.
- `degraded/` — deterministic degradations of clean cells + real failures.

Golden sources are not corpus entries: `clean/gs1-fnc1.png`, `eci/` (ECI
and Hanzi symbols) and `payloads/` (one symbol per payload kind) are
written by `gen-fixtures` from explicit segment streams, and
[`../spec/README.md`](../spec/README.md) § Examples names the golden each
one produces. The exact-content inputs under `symbology/` are listed in
[`symbology/CONTENT-FIXTURES.md`](symbology/CONTENT-FIXTURES.md).

`cargo run -p xtask -- corpus-report` runs the scanner over the corpus and
fails on any regression; `--write` refreshes the README results block.
