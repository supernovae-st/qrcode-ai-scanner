# Binding repo + CI architecture — dev-ready plan

> **Date** 2026-06-17 · **Repo** `github.com/supernovae-st/qrcode-ai-scanner` (PUBLIC OSS, AGPL-3.0-or-later)
> **Goal** host one Rust core + 8 language bindings cleanly in a single repo, with
> a contract that cannot drift and a release path where one `vX.Y.Z` tag fans out
> to every registry. **Decision = monorepo, not split repos.** One tag, one
> CHANGELOG, one `ScanReport` contract, one source of version truth.
>
> **Scope today** — rust-lib · cli · node · wasm · python (publishing now).
> **Coming** — flutter (flutter_rust_bridge → pub.dev) · kotlin/android + swift/ios (UniFFI → Maven Central + SwiftPM).

---

## 0 · State of the world (grounded in the repo, 2026-06-17)

```
crates/
  qrcode-ai-scanner       core lib   → crates.io   (workspace member)
  qrcode-ai-scanner-cli   qrscan     → crates.io   (workspace member)
  qrcode-ai-scanner-node  napi v3    → npm         (member · publish=false · cdylib)
  qrcode-ai-scanner-wasm  wasm-bindgen → npm        (member · publish=false · cdylib+rlib)
  qrcode-ai-scanner-py    PyO3/maturin → PyPI        (EXCLUDED from workspace)
xtask/                    corpus-report etc.        (workspace member, publish=false)
bindings/report-types.d.ts   the TS mirror of ScanReport (synced into npm pkgs)
spec/scan-report.schema.json the JSON-Schema SSOT (CI-validated golden examples)
scripts/check-type-parity.py the existing drift gate (Rust↔TS↔Schema)
scripts/sync-report-types.mjs propagates the TS contract into npm packages
.github/workflows/  ci.yml · deep-checks.yml · npm-publish.yml · python.yml
```

**Workspace version SSOT** = `[workspace.package].version = "0.3.0"` in root `Cargo.toml`.
Every cargo member inherits via `version.workspace = true`. The py crate is excluded,
so it hardcodes `0.3.0` (the drift point we fix in §3).

**Two membership patterns already in play, and they encode the rule:**
- A binding whose `cargo`-native build (clippy/test `--workspace`) is healthy → **member** with `publish = false` (node, wasm). It compiles as a `cdylib` under normal cargo; CI lints it for free.
- A binding built by a **non-cargo tool that drives its own build** (maturin) → **excluded**, with its own workflow covering lint/test (python). It can't sit in `--workspace` cleanly because maturin owns the build graph + needs its own lockfile.

That existing split is the decision rule for every new binding. We just apply it.

---

## 1 · Where each new binding lives (membership + dir layout)

### 1.1 Decision rule (one sentence)

> A binding crate is a **cargo workspace member** iff `cargo {clippy,test} --workspace`
> can build it as-is with stable cargo. If a foreign build tool (maturin, flutter_rust_bridge
> codegen, uniffi-bindgen) must drive the build or needs its own lockfile/codegen step, the
> crate is **`exclude`d** and gets its own CI workflow — exactly like `-py` today.

### 1.2 Per-binding verdict

| Binding | Rust crate | Member or excluded | Why |
|---|---|---|---|
| **uniffi (kotlin + swift)** | `crates/qrcode-ai-scanner-ffi` | **MEMBER** (`publish = false`, `crate-type = ["cdylib","staticlib","lib"]`) | uniffi is *cargo-native*: it's a normal `cdylib`/`staticlib` build + a `uniffi-bindgen` post-step that reads the compiled lib. `cargo build` works unmodified, so `--workspace` lints it for free. The bindgen step (generating `.kt`/`.swift`) runs in CI after build, not during it. **One FFI crate serves BOTH kotlin and swift** (uniffi is multi-target from one `.udl`/proc-macro surface). |
| **flutter** | `crates/qrcode-ai-scanner-flutter` | **EXCLUDED** (like `-py`) | flutter_rust_bridge runs a `flutter_rust_bridge_codegen` step that generates Rust *and* Dart glue and expects to own the build invocation (`cargo ndk` / `cargo-xcode` cross-compile driven from the Flutter side). It also pulls a heavier dep tree (`flutter_rust_bridge` runtime). Excluding keeps `cargo test --workspace` fast and avoids the codegen step polluting the core lint job — mirrors exactly why `-py` is excluded. |

> Net: **one new member** (`-ffi`), **one new excluded** (`-flutter`). Both
> consume the core via `path = "../qrcode-ai-scanner", features = ["serde"]` —
> the same edge node and wasm already use. No binding ever depends on another binding.

### 1.3 Non-Rust package sides — directory convention

The Rust crate is the *engine* of a binding; the **idiomatic package** (Dart pubspec,
Gradle/AAR, SwiftPM) is the *shell* a user installs. Today node/wasm keep their JS shell
*inside* the crate dir (`crates/qrcode-ai-scanner-node/package.json`) because napi/wasm-bindgen
emit JS *next to* the compiled artifact — that's correct and stays.

For the new ecosystems the package shell is large and idiomatic-rooted (Gradle wants a
project root, SwiftPM wants `Package.swift` at a repo-relative root, Flutter wants a plugin
package layout). Putting those *inside* `crates/` would fight each toolchain. **Convention:**

```
crates/qrcode-ai-scanner-ffi/        # the uniffi Rust crate (member) — the .udl / proc-macro surface lives here
crates/qrcode-ai-scanner-flutter/    # the flutter_rust_bridge Rust crate (excluded)

bindings/                            # NON-Rust package shells (idiomatic roots), one subdir per ecosystem
  kotlin/                            #   Gradle project → Maven Central (io.github.supernovae-st / com.qrcode-ai)
    build.gradle.kts · settings.gradle.kts · src/main/kotlin/ (generated .kt lands here)
  swift/                             #   SwiftPM package → SwiftPM registry / tagged GitHub
    Package.swift · Sources/QrcodeAiScanner/ (generated .swift + xcframework)
  report-types.d.ts                  #   (existing — the TS contract SSOT, unchanged)

flutter/                             # Flutter plugin package → pub.dev (flutter wants a clean top-level package root)
  pubspec.yaml · lib/ (generated Dart) · android/ ios/ (platform glue) · rust → ../crates/qrcode-ai-scanner-flutter
```

**Rationale for the asymmetry (documented so it doesn't read as inconsistency):**
- `node`/`wasm` JS shells stay **in-crate** — the build tool emits JS beside the artifact; moving it breaks `napi`/`wasm-pack` path assumptions.
- `kotlin`/`swift` shells go under **`bindings/<lang>/`** — they're consumed by foreign package managers that want a project root, and uniffi-bindgen writes generated source *into* them.
- `flutter` gets a **top-level `flutter/`** — pub.dev + the Flutter plugin template strongly assume a package at a discoverable root; nesting it under `bindings/` is supported but fights `flutter create --template=plugin` ergonomics. One exception, called out, beats a forced-uniform layout that every Flutter dev fights.

`bindings/` is therefore "the contract + the foreign-package-manager shells"; `crates/`
is "everything cargo builds". `.gitignore` the generated `*.kt`/`*.swift`/Dart glue +
`*.xcframework` (regenerated in CI from the crate — git is not the artifact store).

---

## 2 · The canonical binding contract (the anti-drift core)

### 2.1 The surface every binding MUST expose, identically

Two functions + one return type. Names adapt to each language's casing; **semantics are frozen**.

```
scan(bytes: &[u8], profile: string = "full")        -> ScanReport            # encoded image (PNG/JPEG/WebP/GIF)
scan_frame(rgba: &[u8], w: u32, h: u32, profile = "frame") -> ScanReport     # raw RGBA8 camera frame, no PNG roundtrip
version() -> string                                                          # the core semver
```

- `profile` ∈ `{ "full", "fast", "frame" }` (the `ScanProfile` enum, parsed from a string at every boundary — see `parse_profile` in `-py`/`-node`/`-wasm`).
- Return is the **`ScanReport`** wire object defined by `spec/scan-report.schema.json`
  (draft 2020-12) — `detections[]`, `score{}`, `hints[]`, `payloads`, `engines[]`,
  `versions{scanner,pipeline,score_contract}`, ISO-15415 card. "No QR found" is a
  **valid report** (empty `detections`), never an error.
- Error model = the `QRS-xxx` catalog (`spec/03-errors.md`): map to each language's
  idiom (PyErr / JS throw / Kotlin exception / Swift `throws` / Dart exception),
  but the **code + message stem are identical** across surfaces.

Every binding may add **convenience** wrappers (async `scan()` in node, typed `scanJson`,
Pillow-image overload in py) — but the canonical 3 above MUST exist and MUST return the
schema-valid `ScanReport`. Convenience is additive; the contract is the floor.

### 2.2 SSOT layering (who is the source of truth)

```
        Rust serde types (crates/qrcode-ai-scanner/src/{report,payload}.rs)
                              │  (serialize)
                              ▼
        spec/scan-report.schema.json   ←──  THE WIRE SSOT (JSON-Schema 2020-12)
            │                                 │
   spec/examples/*.json                bindings/report-types.d.ts
   (golden, real-binary output)        (TS mirror, shipped into npm)
```

- **Schema is the wire SSOT.** All bindings serialize the *same* Rust `ScanReport`
  to JSON and hand JSON (or a deserialized native object) across the boundary —
  no binding re-implements a single field. This is already how node/wasm/py work
  (`scan_json` → `serde_json::to_string` → parse on the JS/Py side). uniffi + flutter
  do the same: return the JSON string (or a uniffi-`Record` mirror generated from one
  source — see below), never a hand-maintained struct.

### 2.3 How to keep it from drifting — the contract test (extend the existing gate)

The precedent is **`scripts/check-type-parity.py`** (runs in `ci.yml` `test` job): it
compares Rust enums ↔ TS unions ↔ schema `$defs`, exit 1 on ANY divergence. We
**extend the same gate** — it already parses Rust enum variants generically.

1. **Keep** the existing Rust↔TS↔Schema enum parity (payload kinds, hints, engines, charsets, grades, axes).
2. **ADD a cross-binding smoke-contract test** (`scripts/check-binding-contract.py`, run in CI per binding once built): for a fixed fixture image, every binding's `scan()` output, parsed as JSON, MUST validate against `spec/scan-report.schema.json` AND match the golden `spec/examples/clean-url.json` on the stable fields (text, symbology, score band — redact timing/ms). This is the "all 7 surfaces agree" test. One fixture, N bindings, one schema. If a binding drops a field, it fails.
3. **For uniffi/flutter typed mirrors** (if we expose a typed `ScanReport` Record rather than a JSON string): generate the uniffi `.udl` Record / Dart class **from the JSON Schema** (codegen step), and assert the generated mirror's field set == schema field set in the same gate. Never hand-write the mirror. (Default recommendation: **return the JSON string + a thin typed parser** for v1 of each new binding — zero mirror to drift — and only graduate to a typed Record when a binding's users demand it.)
4. **Golden examples stay CI-validated** (`tests/spec_golden.rs` already deserializes + schema-validates every `examples/*.json`). A field added to the report without updating schema+examples already breaks the build.

> Drift is structurally impossible: a new field must land in (a) Rust type, (b) schema
> `$defs`, (c) TS mirror, (d) a golden example — or one of four CI jobs goes red. New
> bindings plug into the **same** four checks; they add a row, not a new mechanism.

---

## 3 · Version sync (one core 0.x → every package manifest)

**SSOT** = `[workspace.package].version` in root `Cargo.toml`. Problem: non-cargo manifests
(`package.json` ×3, `pubspec.yaml`, Gradle `version`, `Package.swift` tag, `pyproject` via
maturin-dynamic) don't inherit it. Today node/wasm `package.json` hardcode `"0.3.0"` and the
py crate hardcodes `version = "0.3.0"` — **three+ drift points**.

### 3.1 Mechanism — `xtask sync-version` (the precedent is `xtask`, already a member)

Add a subcommand to the existing `xtask` crate (same place `corpus-report` lives — it already
edits README in place). It reads the workspace version once and **writes it into every
foreign manifest**, deterministically:

```
cargo run -p xtask -- sync-version          # writes the canonical version everywhere
cargo run -p xtask -- sync-version --check   # exit 1 if any manifest is out of sync (CI gate)
```

Targets it rewrites:
| File | Field | How |
|---|---|---|
| `crates/qrcode-ai-scanner-node/package.json` | `.version` | JSON edit |
| `crates/qrcode-ai-scanner-wasm` pkg (generated) | `.version` | already patched by `patch-wasm-pkg.mjs` — feed it the version |
| `crates/qrcode-ai-scanner-py/Cargo.toml` | `package.version` | TOML edit (excluded crate, can't inherit) |
| `flutter/pubspec.yaml` | `version:` | YAML edit |
| `bindings/kotlin/build.gradle.kts` | `version = "x"` | regex/line edit |
| `bindings/swift/Package.swift` | (tag-driven; SwiftPM versions = git tags) | **no edit** — the `vX.Y.Z` tag IS the version |

- `pyproject.toml` already uses `dynamic = ["version"]` via maturin → it reads the
  py crate's `Cargo.toml` version, so syncing that one TOML field is enough for PyPI.
- SwiftPM consumes the **git tag** directly — no manifest field to sync (one less thing to drift).
- `--check` runs in `ci.yml` (cheap, no build) so a bump that forgets a manifest fails PR CI.

### 3.2 Bump flow (single human action)

Use **`release-plz`** (Rust-native, conventional-commits, workspace-aware — confirmed via
docs) in **single-tag mode** so the workspace gets ONE version + ONE tag:

```toml
# release-plz.toml
[workspace]
git_tag_enable    = false   # disable per-crate tags
git_release_enable = false
# bump all workspace members together to one version
dependencies_update = true

[[package]]
name = "qrcode-ai-scanner"     # the anchor package owns the single tag
git_tag_name    = "v{{ version }}"
git_tag_enable  = true
git_release_enable = true

[[package]]
name = "qrcode-ai-scanner-cli" # also publishes to crates.io
# (node/wasm/py have publish=false in Cargo.toml → release-plz skips cargo publish for them,
#  but still bumps their version — exactly the doc-confirmed behavior)
```

`release-plz` opens a **Release PR** that bumps `[workspace.package].version`, updates
`CHANGELOG.md`, and (via a PR-time hook) runs `xtask sync-version` so the foreign manifests
land in the same PR. Merging that PR → push tag `vX.Y.Z` → §4 fan-out fires. crates.io
publish (core + cli) is done by `release-plz release` on the merge; the *binding registries*
publish off the tag.

> Why release-plz over cargo-release/manual: it's workspace-version-aware, generates the
> changelog from conventional commits, and its `publish=false` semantics already match our
> node/wasm/py crates (bumps version, skips `cargo publish`). cargo-release works too but
> needs more hand-wiring for the foreign-manifest step; release-plz gives us the Release-PR
> review gate for free.

---

## 4 · CI / release orchestration — one tag fans out to all registries

### 4.1 Shape: a thin `release.yml` orchestrator + per-binding **reusable** workflows

**Decision: reusable workflows (`workflow_call`), called by a single tag-triggered orchestrator** — NOT one monolithic release workflow, NOT N independently-tag-triggered workflows.

- **Independently-tag-triggered** (today's `npm-publish.yml` + `python.yml` both fire on `tags: ["v*"]`) works but: no single place to see "did the whole release go out?", no shared gate, duplicated tag logic, and partial-failure recovery is per-file. It scales badly at 8 bindings.
- **Monolith** = one giant file, no reuse, hard to test a single binding's publish.
- **Reusable + orchestrator** = each binding's publish logic is a `workflow_call` file (testable in isolation via `workflow_dispatch`), and ONE `release.yml` fans them out on the tag, with a pre-flight gate that must pass before any publish.

```
.github/workflows/
  ci.yml                 # unchanged role: lint + test + type-parity on every PR/push (the floor)
  deep-checks.yml        # unchanged: weekly mutants + fuzz
  release.yml            # NEW orchestrator — triggers on tag v*, fans out (the only tag-triggered file)
  _publish-crates.yml    # reusable (workflow_call): release-plz release → crates.io (core + cli)
  _publish-npm.yml       # reusable: napi matrix + wasm  (lift today's npm-publish.yml body)
  _publish-pypi.yml      # reusable: maturin matrix + OIDC trusted publish (lift today's python.yml wheels/release)
  _publish-maven.yml     # reusable (coming): uniffi → kotlin AAR → Maven Central
  _publish-swiftpm.yml   # reusable (coming): uniffi → xcframework → SwiftPM (tag/registry)
  _publish-pub.yml       # reusable (coming): flutter_rust_bridge → pub.dev
```

### 4.2 The orchestrator (skeleton)

```yaml
name: release
on:
  push:
    tags: ["v*"]
  workflow_dispatch:        # manual re-run of the whole fan-out

permissions:
  contents: write           # tag → github release
  id-token: write           # PyPI OIDC trusted publishing

jobs:
  preflight:                # ONE gate before anything publishes
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: python3 scripts/check-type-parity.py        # contract gate
      - run: cargo run -p xtask -- sync-version --check    # version-sync gate
      - run: cargo nextest run --workspace                 # the floor must be green at the tagged SHA

  crates:  { needs: preflight, uses: ./.github/workflows/_publish-crates.yml, secrets: inherit }
  npm:     { needs: preflight, uses: ./.github/workflows/_publish-npm.yml,    secrets: inherit }
  pypi:    { needs: preflight, uses: ./.github/workflows/_publish-pypi.yml,   permissions: { id-token: write } }
  # coming:
  # maven:   { needs: preflight, uses: ./.github/workflows/_publish-maven.yml,   secrets: inherit }
  # swiftpm: { needs: preflight, uses: ./.github/workflows/_publish-swiftpm.yml, secrets: inherit }
  # pub:     { needs: preflight, uses: ./.github/workflows/_publish-pub.yml,     secrets: inherit }
```

### 4.3 Invariants carried into every reusable workflow

- **Idempotent publish** (already the node/wasm pattern: `npm view …@$VER` skip; py uses `skip-existing: true`). Every reusable workflow MUST be safe to re-run after partial failure — re-running `release.yml` via `workflow_dispatch` recovers a half-published release with zero manual flags. uniffi/flutter workflows inherit the same "check-then-skip" rule.
- **Matrix lives inside the reusable workflow** (napi 6-target, maturin 6-platform), so the orchestrator stays flat. The orchestrator is fan-out only.
- **Build vs publish split stays** (npm/py already split build-matrix → upload-artifact → single publish job). Keep it: a target that fails to *build* never half-publishes.
- **Secrets**: `NPM_TOKEN` (npm), PyPI = OIDC no-token, `CARGO_REGISTRY_TOKEN` (crates via release-plz), Maven = `OSSRH_*` + GPG signing key (coming), SwiftPM = git-tag only (no secret), pub.dev = OIDC or `PUB_TOKEN` (coming). Pass via `secrets: inherit`.

> One `git tag v0.4.0 && git push --tags` → preflight gate → crates.io + npm + PyPI in
> parallel (then Maven/SwiftPM/pub.dev as they land). Failure isolation per registry; the
> contract+version gate runs ONCE up front.

---

## 5 · README install-matrix rows

Replace the current 4-row table (and the "four ways to use it" line → "**N ways**"). Add
**python now**; add flutter/kotlin/swift as a **"Coming soon"** sub-section so the matrix
advertises the roadmap without linking dead packages.

**Add now (live):**

| Surface | Package | Install | One-liner |
|---|---|---|---|
| 🐍 **Python** | [`qrcode-ai-scanner`](https://pypi.org/project/qrcode-ai-scanner/) | `pip install qrcode-ai-scanner` | `scan(img_bytes, "full")` → `dict` (schema-valid `ScanReport`) |

**Add as "Coming soon" (roadmap, no link until published):**

| Surface | Package (planned) | Registry | Status |
|---|---|---|---|
| 🎯 **Flutter / Dart** | `qrcode_ai_scanner` | pub.dev | flutter_rust_bridge — phase D |
| 🤖 **Kotlin / Android** | `com.qrcode-ai:scanner` (AAR) | Maven Central | UniFFI — phase E |
| 🍎 **Swift / iOS** | `QrcodeAiScanner` (SwiftPM) | SwiftPM (tag) | UniFFI — phase E |

Also update the **Workspace** status table: add `-py` row (v0.3 — release-ready / publishing),
and later `-ffi` (uniffi, kotlin+swift) and `-flutter` rows.

---

## 6 · Phased rollout order

| Phase | Deliverable | Depends on | Why this order |
|---|---|---|---|
| **A · Foundation** | `xtask sync-version` (+`--check`) · `release-plz.toml` single-tag · extend `check-type-parity.py` with the cross-binding smoke-contract test | nothing | Lock version-SSOT + contract gate BEFORE adding bindings, so every new binding plugs into a finished frame. |
| **B · Orchestrator** | Refactor `npm-publish.yml`+`python.yml` → `_publish-npm.yml`/`_publish-pypi.yml` reusable + `_publish-crates.yml` + `release.yml` orchestrator with preflight gate | A | Prove the fan-out shape on the 3 bindings we already ship before adding new ones. No new binding logic — pure refactor, low risk. |
| **C · Ship python** (already in flight) | PyPI trusted-publishing live · README python row · workspace table row | A, B | Closes the binding currently mid-publish; first real exercise of `release.yml`. |
| **D · Flutter** | `crates/qrcode-ai-scanner-flutter` (excluded) · `flutter/` package · `_publish-pub.yml` · pub.dev · README row live | A, B | Single-target (Dart) — simpler than the dual-target uniffi; flutter_rust_bridge is the most self-contained of the three. |
| **E · UniFFI (kotlin + swift)** | `crates/qrcode-ai-scanner-ffi` (member) · `bindings/kotlin/` + `bindings/swift/` · `_publish-maven.yml` + `_publish-swiftpm.yml` · README rows live | A, B | Last because it's two registries (Maven Central onboarding + GPG signing is the slowest external dependency) from one crate; do the dual-target once the orchestrator + contract gate are battle-tested on D. |

Within each binding phase: (1) crate + package shell, (2) wire into preflight contract test, (3) wire `xtask sync-version`, (4) reusable publish workflow tested via `workflow_dispatch`, (5) add to orchestrator + README, (6) first tagged release.

---

## 7 · RISKS

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| R1 | **AGPL vs app-store distribution.** Static-linking the AGPL core into a shipped iOS/Android app (Maven AAR, SwiftPM xcframework, Flutter plugin) triggers AGPL §13 / GPL linking obligations for the *whole app* — a real adoption blocker for closed-source mobile apps, and App Store / Play Store AGPL friction is well documented. | **HIGH** | (a) Document loudly in each mobile binding's README that consuming app must be AGPL-compatible OR hold a commercial license; (b) **offer a commercial dual-license** for closed-source mobile distribution (SuperNovae owns the copyright — this is the standard MongoDB/Qt path and aligns with the qrcode-ai.com product consuming it); (c) gate the mobile phases (D/E) on a licensing decision recorded in `dx/state/decisions.yaml`. The crates.io/npm/PyPI surfaces are server/tooling-side where AGPL is far less frictional — ship those first (already the phase order). |
| R2 | **Contract drift on a new typed mirror.** If uniffi/flutter expose a hand-written typed `ScanReport`, it silently diverges from the schema. | MED | v1 of every new binding returns the **JSON string + schema** (zero mirror). Typed Records only via codegen-from-schema, asserted in the parity gate (§2.3). |
| R3 | **Workspace-member uniffi slows core CI.** Adding `-ffi` as a member adds a `cdylib`/`staticlib` to `cargo test --workspace`. | LOW | Keep `-ffi` minimal (it just re-exports `scan`/`scan_frame` + JSON); uniffi-bindgen runs in the publish workflow, not in `ci.yml`. If build time bites, move it behind a CI matrix split, not out of the workspace. |
| R4 | **Version-sync forgotten on a manual bump.** Someone bumps `Cargo.toml` by hand, skips foreign manifests. | MED | `xtask sync-version --check` is a **preflight gate** in `release.yml` AND a job in `ci.yml` — a desynced manifest fails PR CI, never reaches a tag. |
| R5 | **Partial publish (one registry fails mid-fan-out).** | MED | Every reusable workflow is idempotent (check-then-skip, already proven on npm/py); re-running `release.yml` via `workflow_dispatch` recovers with no flags. Per-registry failure isolation (independent jobs) means npm failing doesn't block PyPI. |
| R6 | **Maven Central / GPG onboarding latency.** Sonatype namespace verification + GPG key publishing is slow + external. | LOW (schedule) | Phase E is last; start the Sonatype namespace request (`io.github.supernovae-st`) at the START of phase B so it's verified by the time E begins. |
| R7 | **rxing wasm/chrono + binaryen pin fragility** (already encountered — apt binaryen too old, wasm-opt disabled). New cross-compile targets (Android NDK, iOS) hit similar toolchain-version traps. | MED | Pin every cross-toolchain version explicitly in the reusable workflow (the binaryen-130 pin is the precedent); never rely on runner defaults for wasm/NDK/binaryen. |
| R8 | **napi v3 / maturin / uniffi major-version churn** across 8 bindings. | LOW | Bindings are independent; a tool bump touches one reusable workflow + one crate. The contract gate guarantees a tool bump that changes output shape fails CI before publish. |

---

## Appendix · grounded references (read before implementing)

- Membership precedent: root `Cargo.toml` `members`/`exclude`; `-node`/`-wasm` `publish = false` + `cdylib`; `-py` excluded + own `python.yml`.
- Contract gate precedent: `scripts/check-type-parity.py` (Rust↔TS↔Schema, generic enum parser — extend it).
- Version-propagation precedent: `xtask` (corpus-report edits README in place) + `scripts/patch-wasm-pkg.mjs` (already patches wasm pkg version).
- Idempotent publish precedent: `npm-publish.yml` (`npm view …@$VER` skip; napi self-detecting `--skip-optional-publish`) + `python.yml` (`skip-existing: true`, OIDC trusted publishing).
- TS contract sync precedent: `scripts/sync-report-types.mjs` (copies `bindings/report-types.d.ts` into both npm packages; runs from any cwd).
- Reusable-workflow + matrix-fan-out + `secrets: inherit`: GitHub Actions docs (context7 `/websites/github_en_actions`).
- Single-tag workspace release: release-plz `[[package]] git_tag_name = "v{{ version }}"` + `git_tag_enable=false` workspace default + `publish=false` skip semantics (context7 `/release-plz/release-plz`).
