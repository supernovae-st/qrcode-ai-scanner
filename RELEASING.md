# Releasing

The proven runbook — three releases shipped with it on 2026-07-08 alone
(0.6.0 · 0.7.0 · 0.7.1). Every step exists because skipping it bit us once;
the dates in parentheses point at the incident.

## 0 · Pick the version

- **Patch (0.x.Y)** — additive-only: new options, new tests, docs.
- **Minor (0.X.0)** — anything 0.x-breaking: a public type loses a trait
  (`Copy` on `ScanConfig`, 0.7.0), a signature changes, wire semantics move.
- The wire report itself is additive-only forever; semantic score changes
  bump `score_contract` inside the report, not just the crate version.

## 1 · Bump — 8 surfaces, one commit

`version = "X.Y.Z"` in: workspace `Cargo.toml` · `bindings/flutter/rust/Cargo.toml`
· `crates/qrcode-ai-scanner-py/Cargo.toml` · `crates/qrcode-ai-scanner-uniffi/Cargo.toml`
· the core pin inside `crates/qrcode-ai-scanner-cli/Cargo.toml` ·
`bindings/flutter/pubspec.yaml` (`version:`) ·
`bindings/kotlin/qrcodeaiscanner/build.gradle.kts` ·
`crates/qrcode-ai-scanner-node/package.json`.

Then regenerate the FIVE lockfiles (workspace, py, uniffi, flutter/rust,
fuzz): `cargo update -w --manifest-path <m> --offline` each. A forgotten
mirror is caught by the mobile.yml version gate at tag time — but catching
it locally saves a round trip (replicate: the `v()` one-liners in
mobile.yml's "versions agree" step).

**Version-pinned docs ride the same commit**: the JitPack coordinate in
`bindings/kotlin/README.md` AND the root `README.md` mobile paragraph
(left at v0.6.0 through two releases, 2026-07-08 — greppable:
`grep -rn "qrcode-ai-scanner:v0" README.md bindings/`).

## 2 · Cut the changelog

`## Unreleased` → `## X.Y.Z — YYYY-MM-DD`. The tag-time gate REQUIRES a
dated section matching the workspace version (mobile.yml, shipped after
0.5.0 stale-notes) — a tag without it fails `test` before any publish.

## 3 · Pre-tag gates, locally first

```bash
cargo fmt --all --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features
cargo nextest run --workspace && cargo test --doc --workspace
python3 scripts/check-type-parity.py
cargo +1.88 check -p qrcode-ai-scanner --all-features --locked   # MSRV
```

Foreign trees when their code changed: `cargo test --manifest-path
crates/qrcode-ai-scanner-{py,uniffi}/Cargo.toml`.

## 4 · Push, then gate CI **per-workflow-latest** before tagging

Push the release commit and wait for CI. **Judge each workflow by its most
recent run on the commit, never by grepping the full run list**: pushes can
spawn duplicate runs, and an infra-flake first attempt reads as red beside
its green retry (the sccache flake nearly blocked — and then mis-gated —
0.7.0, 2026-07-08):

```bash
for wf in ci python flutter mobile; do
  gh run list --commit "$(git rev-parse HEAD)" --workflow=$wf --limit 1 \
    --json conclusion --jq '.[0].conclusion'
done   # four lines of "success", or no tag
```

## 5 · Tag + push the tag

Annotated tag `vX.Y.Z` (headline + expected-red note), `git push origin vX.Y.Z`.

## 6 · What fires, and what to expect (state: 2026-07-08)

| Leg | Expectation |
|---|---|
| crates-publish · npm-publish · python | green; registries live in ~10-15 min |
| mobile › test + android | green (JitPack chain proven since v0.6.0) |
| mobile › ios | **expected red** at the dev-mode guard until the one-time Xcode leg (`bindings/swift/release.sh`) runs |
| flutter › publish to pub.dev | **expected red** until the one-time manual first publish |
| JitPack | builds on demand — trigger with a GET on the artifact URL |

## 7 · The steps no pipeline does

- **Create the GitHub Release by hand** (`gh release create vX.Y.Z
  --notes-file <changelog-section>`): the ios leg that would create it dies
  at its own guard by design — nobody else will (three hand-created
  releases on 2026-07-08).
- Verify registries: `npm view @supernovae-st/qrcode-ai-scanner-wasm
  version` (this is the builder's bump signal) + crates.io + PyPI.
- Downstream: the landing/app pins a caret 0.x range — **the caret freezes
  the minor** (`^0.7.0` never takes 0.8.0); minor bumps need a manual edit
  in the consumer.
