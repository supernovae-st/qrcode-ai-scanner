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
python3 scripts/check-publish-guards.py --self-test   # both workflow policies' own scenarios
python3 scripts/check-publish-guards.py               # the publishing-job policy (§ Publishing)
python3 scripts/check-publish-guards.py --toolchains  # one Rust toolchain (§ Toolchain)
python3 scripts/package-inspect.py selftest           # the archive inspector's own scenarios
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

## 6 · What fires, and what to expect (state: 2026-10-10)

Every publish leg is **idempotent** (skips versions already on the
registry) and **tag-only** (§ Publishing): a run on anything but a
`refs/tags/v*` ref builds and inspects, never publishes. If a leg dies on a
transient failure, re-run its failed jobs (`gh run rerun <run-id> --failed`)
or dispatch the workflow ON THE TAG (`gh workflow run <name> --ref vX.Y.Z`):
the tag's own commit and workflow file run again, nothing double-publishes,
and a publishing job waits for one of the same workflow and tag still
running (one concurrency group per workflow and tag). pub.dev is the
exception: it accepts only a pushed tag, so flutter › publish re-runs as
failed jobs only. npm-publish › publish-native uploads each platform package
missing at this version and the main package only once every one it pins
resolves on the registry: a re-run uploads exactly what is still missing,
and a version npm calls "previously published" (a probe that still answered
404 after an earlier upload) counts as live. crates-publish compares each
upload with the archive it inspected; a difference stops the job before the
next crate, with that crate live (yank it if it must not stay).
A defect in the workflow itself can no longer be fixed on main and
re-dispatched from main (that is how 0.9.0's Node packages came to be built
from post-tag commit 9f8bbcf, 2026-07-20): fix it on main and cut the next
patch tag. And the release tooling itself is **pinned exact** (§ Toolchain):
the v0.9.0 train lost its first two npm-publish runs to an unpinned caret
resolving a new CLI whose host validation rejected `--use-napi-cross`
outside Linux-gnu (2026-07-20) — a toolchain must never move under a tag.

| Leg | Expectation |
|---|---|
| crates-publish · npm-publish · python | green; registries live in ~10-15 min — once the one-time setup is done (§ Publishing). Before it, crates-publish and npm-publish stop at authentication; with only some npm trusted publishers configured, the configured platform packages go up and the main package does not, until a re-run after the rest are configured |
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
from a branch included — stops before the registry: crates-publish,
npm-publish and python build their artifacts and read them back
(`scripts/package-inspect.py`), mobile and flutter build theirs. Each
publishing job is its own job, guarded by `if: startsWith(github.ref,
'refs/tags/v')`, needing the job that inspects what it uploads, holding the
only write-capable permission in its workflow, naming a GitHub environment
and running the versions gate first.

`scripts/check-publish-guards.py` (ci › lint) holds the workflows to these
rules, and to the ones below. It is a static lint over the files in
.github/workflows of the revision under test: it reads their YAML and
matches the commands of every `run:` script, so it fails a change that drops
a rule in a form it can read. It does not see what a script or a tool a step
calls goes on to do, the workflows of another branch or tag, or a repository
setting. The environments' deployment rules and the tag ruleset (owner
steps 0 and 1) backstop what it misses.

**What that guarantees, and what it does not.** The `if:` guards prevent
accidental publishing by this revision of the workflows; they are not the
security boundary. A workflow file is code on a ref: `workflow_dispatch` runs
the file of the ref it is given, and anyone who can push a branch can push a
copy without the guard. The crates.io, npm and PyPI trusted publishers match
the repository, the workflow file name and the environment, not the ref
(pub.dev alone also checks the pushed tag). The security boundary is the
environments' deployment policy (`v*` tags only) together with the tag
ruleset (who may create a `v*` tag): owner steps 0 and 1 below. And "no
branch dispatch can publish" holds only for revisions that contain this
change: every branch and tag cut before it (v0.1.0 to v0.9.0 included) still
carries the old jobs, which publish on any dispatch with the long-lived
repository tokens — hence step 0, at merge.

| Workflow › job | Environment | Credential | What it uploads |
|---|---|---|---|
| crates-publish › publish | `crates-io` | crates.io trusted publishing: `rust-lang/crates-io-auth-action` trades the OIDC token for a 30-minute crates.io token | both crates, packed (`cargo package --locked --no-verify`) and inspected in the job before the token is minted; each upload compared with its inspected archive, byte for byte |
| npm-publish › publish-native · publish-wasm | `npm` | npm trusted publishing (npm CLI ≥ 11.5.1 on Node 24), provenance attested automatically | the very tarballs pack-native and build-wasm packed and inspected, read back again in the job and uploaded unchanged with `npm publish <tarball> --ignore-scripts` |
| python › release | `release` | PyPI trusted publishing (already live: 0.9.0 carries PEP 740 attestations) | `dist/`, read back in the job: exactly one wheel per row of the wheels matrix and the sdist |
| flutter › publish | `pub` | pub.dev automated publishing; a pushed tag only (pub.dev accepts no other trigger) | the package `package-inspect.py flutter` read back in the job (dormant) |
| mobile › ios-release | `release` | the workflow's own `GITHUB_TOKEN`, `contents: write` | the CI-built xcframework zip only when its sha256 is the one Package.swift pins, compared before the release is created or touched; an attached asset is verified, never replaced |

No registry token is read by any workflow. A token (or a person) is still
required only where a registry has no OIDC path: the first version of a NEW
crate on crates.io, the first version of a NEW npm package (a new napi
target, say — or `npm stage publish`), and pub.dev's first upload, which
must be a user's.

**What a publishing job may run.** A job holding `id-token: write` exposes
the OIDC token request to every one of its steps, and ios-release holds
`contents: write`. In these six jobs (the policy check reads each point):
- every action by commit SHA, first-party actions included, and no local
  action; container and service images by digest; the checkout keeps no
  credential (`persist-credentials: false`); scripts run in bash and
  interpolate no `${{ }}` expression;
- Rust through rustup at `RUST_TOOLCHAIN`, an exact x.y.z resolved step >
  job > workflow and never set in a script; pnpm, Flutter, Dart and every
  other setup action or installer tool at an exact version;
- nothing installed or fetched on demand: no npm/pnpm/yarn install, exec or
  dlx, no pip, uv tool, cargo install or binstall, pub global or dart run;
  the crates go up with `cargo publish --locked --no-verify` (no build
  script runs), and the npm packages with `npm publish <tarball>
  --ignore-scripts` (no lifecycle script runs, nothing is repacked);
- `actions/setup-node` and `actions/setup-python` are the one relaxation:
  pinned by SHA, on a release line (Node 24) or an exact version, never
  `lts/*`, `latest`, `current` or a range. Security point releases of the line
  arrive without a repository change, and the action and its version manifest
  are first-party; a step after the setup prints the resolved versions before
  anything is uploaded, and for npm asserts npm ≥ 11.5.1;
- no secret: no workflow references the `secrets` context at all;
- one publish per workflow and tag at a time (the concurrency group
  `publish-<workflow>-<ref>`, never cancelling);
- one named exception, keyed to three exact steps of flutter › publish: its
  setup-dart and flutter-action steps (stable-channel SDKs) and its `flutter
  pub get` (no lockfile), while the job is dormant (owner step 6 ends it).

The jobs whose outputs the publishing jobs upload (build-native,
pack-native, build-wasm, wheels, sdist, ios) pin their third-party actions by
commit and restore no build cache (rust-cache, sccache) on a tag; they are
not privileged, and the inspections read file sets and texts, not compiled
binaries.

**Named exceptions** to "every distributed archive carries the AGPL text, and
only a tag publishes":
- the iOS xcframework zip (a GitHub release asset, built by mobile › ios or
  bindings/swift/release.sh) holds no LICENSE, unless SwiftPM is shown to
  accept one at the zip root beside the .xcframework;
- JitPack builds the Android AAR from any ref on demand, a branch snapshot
  included, outside Actions (jitpack.yml: Rust 1.88.0, no `--locked`): neither
  the tag guard nor the inspection applies to it.

**Accepted residuals**, each with its reason:
- `pypa/gh-action-pypi-publish` is pinned by commit, but at run time it pulls
  its container image `ghcr.io/pypa/gh-action-pypi-publish:<ref>` by tag, not
  by digest (create-docker-action.py at the pinned commit), inside the job
  holding the OIDC token. pypa controls both the commit and the image; the
  alternative, a hash-locked twine after an explicit OIDC exchange, would
  replace the action PyPI documents.
- The Node build (build-native, pack-native, ci › node-smoke) runs `pnpm
  install` without a lockfile: `@napi-rs/cli` is pinned exact, its own
  dependencies float. Committing a pnpm lockfile needs an install to
  generate it; it is the next step for the package's owner, after which
  these installs take `--frozen-lockfile`.
- The packed binaries (.node, .so, .pyd, .a, .wasm) are built by those
  non-privileged jobs from the tag's checkout; no inspection can tell a
  tampered binary from a sound one, so their pinned actions and the absence
  of caches on a tag are what holds there.

**Owner steps, in this order** (repository and registry settings; no workflow
performs them):

0. **At merge, at once, all of it.**
   - Revoke `CARGO_REGISTRY_TOKEN` at crates.io (Account Settings › API
     Tokens) and `NPM_TOKEN` at npmjs.com (Access Tokens), then delete both
     repository secrets (Settings › Secrets and variables › Actions): nothing
     at this revision reads them, while every older ref's crates and npm jobs
     publish with them on any dispatch.
   - Look for the same two names at the organization and in every
     environment (`gh secret list --org supernovae-st`, `gh secret list --env
     <name>` for each environment): once the repository secret is gone, an
     older workflow's `secrets.NPM_TOKEN` resolves to an organization or
     environment secret of that name. Revoke and delete any found.
   - Add the tag rule `v*` to the existing `release` environment (Settings ›
     Environments › release › Deployment branches and tags › Selected
     branches and tags): it also gates the pre-change python.yml, which
     publishes to PyPI on any dispatch from an older ref.
   - **Tag ruleset.** Settings › Rules › Rulesets › New tag ruleset: target
     `refs/tags/v*`, restrict creations, updates and deletions, with the
     release maintainers alone on the bypass list. Until it exists, anyone
     with write access can create a `v*` tag on any commit, a modified
     python.yml included, and the `release` environment admits it.
   - Retire the stale remote branches: every branch cut before this change
     carries the old publishing jobs (python › release on any dispatch, the
     crates and npm jobs reading the tokens, mobile › ios with `contents:
     write` on any dispatch). List them (`git ls-remote --heads origin`, then
     `git show origin/<branch>:.github/workflows/npm-publish.yml`) and delete
     or archive each one not needed. The tags v0.1.0 to v0.9.0 stay: their
     versions are live, and once the tokens are gone their old jobs fail at
     authentication.
1. **Environments, before steps 2 and 3.** `crates-io`, `npm`, `release` and
   `pub`, each limited to the tag rule `v*` (`release` has it since step 0;
   create the other three). Mandatory before any trusted publisher is
   registered: a referenced environment that does not exist is created on
   first use WITHOUT protection rules, and a publisher bound to an
   unprotected environment accepts a run from any branch. The old `pub.dev`
   environment is no longer referenced and can go.
2. crates.io, for `qrcode-ai-scanner` and for `qrcode-ai-scanner-cli`:
   Settings › Trusted Publishing › add GitHub: owner `supernovae-st`,
   repository `qrcode-ai-scanner`, workflow `crates-publish.yml`,
   environment `crates-io`; then restrict both crates to trusted publishing.
3. npmjs.com, for each of the eight packages —
   `@supernovae-st/qrcode-ai-scanner`, `-darwin-arm64`, `-darwin-x64`,
   `-linux-x64-gnu`, `-linux-arm64-gnu`, `-linux-x64-musl`,
   `-win32-x64-msvc` and `-wasm`: Settings › Trusted Publisher › GitHub
   Actions: organization or user `supernovae-st`, repository
   `qrcode-ai-scanner`, workflow `npm-publish.yml`, environment `npm`; then
   Settings › Publishing access › *Require two-factor authentication and
   disallow tokens*.
4. **Recommended: required reviewers** on `crates-io` and `npm` (with
   *Prevent self-review* where staffing allows), so every publish waits for a
   second person.
5. PyPI: confirm the existing trusted publisher still reads owner
   `supernovae-st`, repository `qrcode-ai-scanner`, workflow `python.yml`,
   environment `release` (unchanged here).
6. pub.dev, once a user has made the first upload. Before its trusted
   publisher is configured, in one change: pin the Flutter and Dart SDKs of
   flutter › publish to exact versions, take the lockfile decision for its
   `flutter pub get` (a tracked pubspec.lock with `--enforce-lockfile`, or a
   recorded no), and delete the named exception from
   scripts/check-publish-guards.py (the check fails on a waiver left with
   nothing to waive). Then Admin › Automated publishing › enable GitHub
   Actions: repository `supernovae-st/qrcode-ai-scanner`, tag pattern
   `v{{version}}`, require environment `pub`.
7. Branch protection: the matrix check `ci / test (ubuntu-latest)` is now
   `test (ubuntu-24.04)`; if it is a required check, update the rule (and
   add `node-smoke`, `wasm-smoke` and `packaging` if they should be).

## Toolchain — one compiler, pinned

Every leg that builds, tests or publishes runs **Rust 1.97.0**:
`RUST_TOOLCHAIN` at the top of each workflow (ci · deep-checks ·
crates-publish · npm-publish · python · mobile · flutter · toolchain-probe).
`ci › lint` runs `scripts/check-publish-guards.py --toolchains`, which reads
the parsed YAML of every *.yml and *.yaml workflow and fails when a
RUST_TOOLCHAIN assignment, at any level (workflow, job or step env), holds
another value or anything but an exact x.y.z; when a script assigns a
toolchain (`RUST_TOOLCHAIN=…`, a `$GITHUB_ENV` write, RUSTUP_TOOLCHAIN); when
FUZZ_TOOLCHAIN is not a dated nightly-YYYY-MM-DD; when a leg names its
toolchain other than through those variables (an action's toolchain or
rust-toolchain input, flow mappings included, a rustup, `cargo +` or
--toolchain argument, a moving channel); or when a rust-toolchain(.toml)
file exists anywhere in the tree, where it would outrank `rustup default`.
The one literal is the MSRV leg, at the `rust-version` the root manifest
declares (pinned by SHA, it says so in its toolchain input).

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
--locked` (scripts/build-wasm.sh), `maturin build --locked`, the wheel python
› inspect builds from the sdist (`MATURIN_PEP517_ARGS=--locked`, against the
Cargo.lock the sdist ships), and cargo-mutants `--cargo-arg=--locked`. A
stale lockfile fails the leg instead of being silently re-resolved. Not
covered: cargo-fuzz (no such flag; it reads the committed fuzz/Cargo.lock),
the cargokit builds, and the Node package's pnpm install in the build and
pack jobs (no pnpm lockfile is committed, so `@napi-rs/cli` is pinned exact
and its own dependencies float: § Publishing, residuals); no publishing job
installs anything.

Pinned build tools: maturin v1.15.0 (`MATURIN_VERSION`, python.yml), also
for the wheel built from the sdist, through a build constraint
(`maturin==1.15.0` in PIP_BUILD_CONSTRAINT and, for pip older than 25.3,
PIP_CONSTRAINT; the wheel's `Generator:` line must name it). The sdist's own
build requirement is `maturin>=1.9.3,<2.0` (the first maturin that puts PEP
639 license files into source distributions); that floor itself is not
exercised in CI. wasm-pack 0.13.1 and cargo-zigbuild 0.23.4
(taiki-e/install-action, SHA-verified); binaryen version_130, checked against
the sha256 binaryen publishes with the release (and scripts/build-wasm.sh
refuses a wasm-opt older than 130); pnpm 10.34.6 through pnpm/action-setup
pinned by commit (v6.0.10); `@napi-rs/cli` 3.7.3 (package.json). The jobs
whose outputs are published take every third-party action by commit
(dtolnay/rust-toolchain, Swatinem/rust-cache, mlugg/setup-zig,
taiki-e/install-action, PyO3/maturin-action), with the release tag (for
dtolnay/rust-toolchain, the branch and the date) in a comment.
