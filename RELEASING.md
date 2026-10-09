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

## 1 · Bump — one command, one commit

Bump the workspace `Cargo.toml` version, then:

```bash
cargo run -p xtask -- sync-version    # mirrors every publish surface + doc pins
```

It rewrites the 8 hand-spelled surfaces (`bindings/flutter/rust/Cargo.toml`
· py · uniffi · the cli core pin · `pubspec.yaml` · `build.gradle.kts` ·
`package.json`) **and** the version-pinned doc coordinates (JitPack lines
in `bindings/kotlin/README.md` + root `README.md`), then prints the
five-lockfile regeneration loop (workspace, py, uniffi, flutter/rust,
fuzz). `--check` verifies without writing (exit 1 on drift) — the local
twin of the mobile.yml version gate, which stays the CI enforcement.

**Version-pinned docs ride the same commit**: the JitPack coordinate in
`bindings/kotlin/README.md` AND the root `README.md` mobile paragraph
(left at v0.6.0 through two releases, 2026-07-08 — greppable:
`grep -rn "qrcode-ai-scanner:v0" README.md bindings/`).

## 2 · Cut the changelog

`## Unreleased` → `## X.Y.Z — YYYY-MM-DD`. The tag-time gates REQUIRE a
dated section matching the workspace version: mobile.yml's version gate
(shipped after 0.5.0 stale-notes) and, since crates, npm and PyPI used to
publish in parallel regardless of it, every publishing job itself —
`scripts/package-inspect.py versions --tag` checks the tag against every
publish surface and the CHANGELOG before the job touches a registry. A tag
without the section publishes nowhere.

## 3 · Pre-tag gates, locally first

On the pinned compiler (§ Toolchain), against the committed lockfiles:

```bash
cargo +1.97.0 fmt --all --check
RUSTFLAGS="-D warnings" cargo +1.97.0 clippy --workspace --all-targets --all-features --locked
cargo +1.97.0 nextest run --workspace --locked && cargo +1.97.0 test --doc --workspace --locked
python3 scripts/check-type-parity.py
cargo +1.88 check -p qrcode-ai-scanner --all-features --locked   # MSRV
```

Foreign trees when their code changed: `cargo +1.97.0 test --locked
--manifest-path crates/qrcode-ai-scanner-{py,uniffi}/Cargo.toml`.

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

Every publish leg is **idempotent** (skips versions already on the
registry) and **tag-only** (§ Publishing): a run on anything but a
`refs/tags/v*` ref builds and inspects, never publishes. If a leg dies on a
transient failure, re-run its failed jobs (`gh run rerun <run-id> --failed`)
or dispatch the workflow ON THE TAG (`gh workflow run <name> --ref vX.Y.Z`):
the tag's own commit and workflow file run again, nothing double-publishes.
A defect in the workflow itself can no longer be fixed on main and
re-dispatched from main (that is how 0.9.0's Node packages came to be built
from post-tag commit 9f8bbcf, 2026-07-20): fix it on main and cut the next
patch tag. And the release tooling itself is **pinned exact** (§ Toolchain):
the v0.9.0 train lost its first two npm-publish runs to an unpinned caret
resolving a new CLI whose host validation rejected `--use-napi-cross`
outside Linux-gnu (2026-07-20) — a toolchain must never move under a tag.

| Leg | Expectation |
|---|---|
| crates-publish · npm-publish · python | green; registries live in ~10-15 min — once the one-time trusted-publisher setup is done (§ Publishing); before it, the publish jobs stop at authentication and nothing half-publishes |
| mobile › test + android | green (JitPack chain proven since v0.6.0) |
| mobile › ios | **expected red** at the dev-mode guard until the one-time Xcode leg (`bindings/swift/release.sh`) runs; `ios-release` is then skipped |
| flutter › publish to pub.dev | **expected red** until the one-time manual first publish (and the package can only build its core once its Rust crate stops depending on a path outside the package) |
| JitPack | builds on demand — trigger with a GET on the artifact URL |

## 7 · The steps no pipeline does

- **Create the GitHub Release by hand** (`gh release create vX.Y.Z
  --notes-file <changelog-section>`): `mobile › ios-release`, which would
  create it, never runs while the ios leg dies at its own guard by design —
  nobody else will (three hand-created releases on 2026-07-08).
- Verify registries: `npm view @supernovae-st/qrcode-ai-scanner-wasm
  version` (this is the builder's bump signal) + crates.io + PyPI.
- Downstream: the landing/app pins a caret 0.x range — **the caret freezes
  the minor** (`^0.7.0` never takes 0.8.0); minor bumps need a manual edit
  in the consumer.

## Publishing — tag-only, trusted, one environment per registry

Only a `refs/tags/v*` ref publishes. Every other run — a `workflow_dispatch`
from a branch included — builds every artifact and inspects it
(`scripts/package-inspect.py`), then stops before the registry. Each
publishing job is its own job, guarded by `if: startsWith(github.ref,
'refs/tags/v')`, holding the only write-capable permission in its workflow
and naming a GitHub environment, so protection rules can be attached:

| Workflow › job | Environment | Credential |
|---|---|---|
| crates-publish › publish | `crates-io` | crates.io trusted publishing: `rust-lang/crates-io-auth-action` trades the OIDC token for a 30-minute crates.io token |
| npm-publish › publish-native · publish-wasm | `npm` | npm trusted publishing (npm CLI ≥ 11.5.1 on Node 24), provenance attested automatically |
| python › release | `release` | PyPI trusted publishing (already live: 0.9.0 carries PEP 740 attestations) |
| flutter › publish | `pub` | pub.dev automated publishing; a pushed tag only (pub.dev accepts no other trigger) |
| mobile › ios-release | `release` | the workflow's own `GITHUB_TOKEN`, `contents: write` |

No registry token is read by any workflow. A token (or a person) is still
required only where a registry has no OIDC path: the first version of a NEW
crate on crates.io, the first version of a NEW npm package (a new napi
target, say — or `npm stage publish`), and pub.dev's first upload, which
must be a user's.

**One-time setup, by the package owner, before the next tag** (until then
the crates and npm publish jobs fail at authentication, before publishing
anything):

1. GitHub › Settings › Environments: create `crates-io`, `npm`, `release`
   and `pub`, each with *Deployment branches and tags* → *Selected* → tag
   rule `v*` (required reviewers optional). A referenced environment that
   does not exist is created on first use WITHOUT protection rules. The old
   `pub.dev` environment is no longer referenced and can go.
2. crates.io, for `qrcode-ai-scanner` and for `qrcode-ai-scanner-cli`:
   Settings › Trusted Publishing › add GitHub: owner `supernovae-st`,
   repository `qrcode-ai-scanner`, workflow `crates-publish.yml`,
   environment `crates-io`.
3. npmjs.com, for each of the eight packages —
   `@supernovae-st/qrcode-ai-scanner`, `-darwin-arm64`, `-darwin-x64`,
   `-linux-x64-gnu`, `-linux-arm64-gnu`, `-linux-x64-musl`,
   `-win32-x64-msvc` and `-wasm`: Settings › Trusted Publisher › GitHub
   Actions: organization or user `supernovae-st`, repository
   `qrcode-ai-scanner`, workflow `npm-publish.yml`, environment `npm`.
4. PyPI: confirm the existing trusted publisher still reads owner
   `supernovae-st`, repository `qrcode-ai-scanner`, workflow `python.yml`,
   environment `release` (unchanged here).
5. pub.dev, once a user has made the first upload: Admin › Automated
   publishing › enable GitHub Actions: repository
   `supernovae-st/qrcode-ai-scanner`, tag pattern `v{{version}}`, require
   environment `pub`.
6. After the first release published through OIDC: delete the
   `CARGO_REGISTRY_TOKEN` and `NPM_TOKEN` repository secrets; on crates.io
   restrict both crates to trusted publishing, on npm set each package's
   publishing access to *Require two-factor authentication and disallow
   tokens*.
7. Branch protection: the matrix check `ci / test (ubuntu-latest)` is now
   `test (ubuntu-24.04)`; if it is a required check, update the rule (and
   add `node-smoke`, `wasm-smoke` and `packaging` if they should be).

## Toolchain — one compiler, pinned

Every leg that builds, tests or publishes runs **Rust 1.97.0**:
`RUST_TOOLCHAIN` at the top of each workflow (ci · deep-checks ·
crates-publish · npm-publish · python · mobile · flutter · toolchain-probe),
and `ci › lint` fails when two workflows disagree or a moving channel
(`rust-toolchain@stable`, `toolchain: stable`, …) comes back.

Why 1.97.0: it is the compiler the PR gate has proven (fmt, clippy
`-D warnings`, the suite on three OSes). Before, every publisher and binding
build took whatever `stable` was that day (1.99.0 on 2026-10-09), so the
compiler that judged a release was not the one that built it. A compiler bump
is one deliberate commit that moves every `RUST_TOOLCHAIN` together, then
reruns the gates, rescue-stress included (f32 warp sampling may move in the
last ulp across compilers, and its gate is same-machine determinism).

Named exceptions, each documented where it lives:
- **MSRV** — `ci › msrv` checks the 1.88 floor that every `rust-version`
  declares; jitpack.yml builds the JitPack AAR with 1.88.0, outside Actions.
- **Fuzzing** — one dated nightly (`FUZZ_TOOLCHAIN` in deep-checks.yml):
  cargo-fuzz needs nightly sanitizers.
- **cargokit** — Flutter device builds (and every consuming app's build) can
  only name the stable / beta / nightly channels; pub.dev ships sources.
- **bindings/swift/release.sh** — builds with the caller's toolchain: run it
  as `RUSTUP_TOOLCHAIN=1.97.0 bindings/swift/release.sh vX.Y.Z`.

`--locked` wherever a lockfile exists: every cargo build, test, run and
publish in the workflows, `napi build -- --locked`, `wasm-pack build --
--locked` (scripts/build-wasm.sh), `maturin build --locked`, and cargo-mutants
`--cargo-arg=--locked`. A stale lockfile fails the leg instead of being
silently re-resolved. Not covered: cargo-fuzz (no such flag; it reads the
committed fuzz/Cargo.lock), the cargokit builds, and the Node package's pnpm
install (no pnpm lockfile is committed, so `@napi-rs/cli` is pinned exact and
its own dependencies float).

Pinned build tools: maturin v1.15.0 (`MATURIN_VERSION`, python.yml), wasm-pack
0.13.1 (taiki-e/install-action, SHA-verified), binaryen version_130 (and
scripts/build-wasm.sh refuses a wasm-opt older than 130), `@napi-rs/cli`
3.7.3 (package.json).
