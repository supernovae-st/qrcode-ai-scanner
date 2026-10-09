#!/usr/bin/env python3
"""Pack what every publish job would ship, then inspect the archives themselves.

Each target is packed or built locally the way its publish job does it, and
the resulting archive is read back: the AGPL LICENSE text (byte-identical to
the root copy, where PEP 639 says it is for Python), the README, the type
files, the version strings, the file set, and the absence of private or stray
files. The file set is exact for npm packages and wheels; a .crate or an sdist
may only hold files git tracks in the directories it packs, plus what cargo
and maturin generate. Nothing is ever uploaded: packing goes through `cargo
package`, `npm pack --pack-destination`, scripts/build-wasm.sh (wasm-pack),
maturin and `pub publish --dry-run`.

  package-inspect.py all [--no-build]          every target this host can run
  package-inspect.py crates [--verify]         the .crate of each published crate
  package-inspect.py node [--platform-dirs D]  the main npm tarball (+ platform packages)
  package-inspect.py wasm [--no-build]         the wasm npm tarball (builds pkg/ first)
  package-inspect.py python                    wheel + sdist through maturin
  package-inspect.py flutter [--flutter BIN]   the pub.dev package
  package-inspect.py archive FILE...           already-built .crate · .tgz · .whl · .tar.gz
  package-inspect.py licenses [--sync]         every LICENSE copy against the root text
  package-inspect.py profiles [--nightly TC]   effective release profile per build root
  package-inspect.py versions [--tag vX.Y.Z]   publish-surface versions (+ the release tag)
  package-inspect.py selftest                  synthetic archives, one defect each: all must fail

Common options: --out DIR (JSON + Markdown receipt and command logs; the
Markdown also lands in $GITHUB_STEP_SUMMARY when that is set) · --strict
(UNAVAILABLE fails too: CI, where every tool must exist) · --cargo-wrapper
PATH (run cargo as `PATH cargo ...`) · --allow-dirty (passed to cargo) ·
--release-set (node · archive: the npm set about to publish, where the main
package pins every napi target and each pin has its platform package).

Verdicts: PASS · FAIL · UNAVAILABLE (a tool or input this host lacks, named,
never a pass) · BLOCKED (a known structural blocker that packaging metadata
cannot fix). Exit 0 when nothing failed (under --strict, also nothing
unavailable) · 1 otherwise · 2 usage. Stdlib only, Python >= 3.11.
"""

import argparse
import contextlib
import dataclasses
import datetime
import email.parser
import hashlib
import io
import json
import os
import pathlib
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile

try:
    import tomllib
except ModuleNotFoundError:  # Python < 3.11
    print("package-inspect.py needs Python >= 3.11 (tomllib)", file=sys.stderr)
    sys.exit(2)

ROOT = pathlib.Path(__file__).resolve().parent.parent
LICENSE = ROOT / "LICENSE"
CANON_TYPES = ROOT / "bindings" / "report-types.d.ts"
NODE_DIR = ROOT / "crates" / "qrcode-ai-scanner-node"
WASM_DIR = ROOT / "crates" / "qrcode-ai-scanner-wasm"
PY_DIR = ROOT / "crates" / "qrcode-ai-scanner-py"
FLUTTER_DIR = ROOT / "bindings" / "flutter"
SPDX = "AGPL-3.0-or-later"

# Every package root whose archive must carry the AGPL text. Each registry
# packs from its own directory, so the root LICENSE never reaches an archive
# by itself: none of the 0.9.0 archives held one.
LICENSE_COPIES = (
    "crates/qrcode-ai-scanner/LICENSE",
    "crates/qrcode-ai-scanner-cli/LICENSE",
    "crates/qrcode-ai-scanner-node/LICENSE",
    "crates/qrcode-ai-scanner-wasm/LICENSE",
    "crates/qrcode-ai-scanner-py/LICENSE",
    "bindings/flutter/LICENSE",
)

PUBLISHED_CRATES = ("qrcode-ai-scanner", "qrcode-ai-scanner-cli")
NODE_NAME = "@supernovae-st/qrcode-ai-scanner"
WASM_NAME = "@supernovae-st/qrcode-ai-scanner-wasm"
PYPI_NAME = "qrcode-ai-scanner"

# Exact npm file sets: the `files` allowlist plus what npm always adds
# (package.json · README · LICENSE). Anything else is a stray.
NODE_FILES = frozenset({
    "LICENSE", "README.md", "package.json",
    "index.js", "index.d.ts", "native.js", "native.d.ts", "report-types.d.ts",
})
WASM_FILES = frozenset({
    "LICENSE", "README.md", "package.json",
    "qrcode-ai-scanner.js", "qrcode-ai-scanner.d.ts", "qrcode-ai-scanner_bg.wasm",
    "report-types.d.ts",
})

# Never in a published archive, whatever its kind; matched case-insensitively
# (Credentials.json leaks as surely as credentials.json). Defence in depth:
# each archive is also held to an exact set or to the files git tracks.
STRAY = tuple((re.compile(pattern, re.IGNORECASE), why) for pattern, why in (
    (r"(^|/)\.qrscan-lane(/|$)", "lane scratch"),
    (r"(^|/)plans/", "planning docs"),
    (r"(^|/)\.env($|\.)|\.env$|(^|/)\.envrc$", "environment file"),
    (r"\.(pem|key|p12|pfx|p8|jks|keystore|gpg|pgp|ppk|kdbx)$", "key material or keystore"),
    (r"(^|/)id_(rsa|dsa|ecdsa|ed25519)(\.pub)?$|(^|/)\.ssh/", "SSH key"),
    (r"(^|/)(\.npmrc|\.yarnrc(\.yml)?|\.pypirc|[._]netrc|\.git-credentials)$", "credentials file"),
    (r"(^|/)(credentials|secrets)[^/]*$", "credentials"),
    (r"(^|/)(target|node_modules|\.dart_tool|__pycache__|mutants\.out[^/]*)/", "build output"),
    (r"(^|/)\.git(/|$)", "git metadata"),
    (r"(^|/)\.(vscode|idea|claude|cursor|codex|aider[^/]*|windsurf|zed|fleet)/", "editor or agent directory"),
    (r"(^|/)(\.DS_Store|Thumbs\.db)$", "OS junk"),
    (r"(\.swp|\.swo|~)$", "editor junk"),
    (r"\.(crate|whl|tgz|zip|tar|gz|bz2|xz|zst|7z|rar|jar|aar|apk|ipa)$", "nested archive"),
))

# What cargo writes into a .crate besides the package's own tracked files.
CARGO_GENERATED = frozenset({".cargo_vcs_info.json", "Cargo.toml.orig", "Cargo.lock"})
# What maturin writes at the sdist root besides the PEP 639 license files.
MATURIN_SDIST_ROOT = frozenset({"PKG-INFO", "pyproject.toml", "README.md"})
# The sdist packs the binding crate and its path dependency side by side.
SDIST_CRATES = {"qrcode-ai-scanner-py/": "crates/qrcode-ai-scanner-py",
                "qrcode-ai-scanner/": "crates/qrcode-ai-scanner"}

# A literal semver in the napi loader's version check: the 0.9.0 loader
# compared every platform package against '0.8.1'.
LOADER_LITERAL = re.compile(r"!== '\d+\.\d+\.\d+'|expected \d+\.\d+\.\d+ but got")
LOADER_RUNTIME_READ = "require('./package.json')"

PASS, FAIL, UNAVAILABLE, BLOCKED = "pass", "fail", "unavailable", "blocked"


@dataclasses.dataclass
class Check:
    name: str
    ok: bool
    detail: str = ""


@dataclasses.dataclass
class Artifact:
    name: str
    kind: str
    path: str = ""
    sha256: str = ""
    size: int = 0
    listing: list = dataclasses.field(default_factory=list)
    checks: list = dataclasses.field(default_factory=list)
    notes: list = dataclasses.field(default_factory=list)
    reason: str = ""
    forced: str = ""  # UNAVAILABLE or BLOCKED, set explicitly
    package: dict = dataclasses.field(default_factory=dict)  # npm: name · version · optionalDependencies

    def check(self, name: str, ok: bool, detail: str = "") -> bool:
        self.checks.append(Check(name, bool(ok), detail))
        return bool(ok)

    def unavailable(self, reason: str) -> "Artifact":
        self.forced, self.reason = UNAVAILABLE, reason
        return self

    def blocked(self, reason: str) -> "Artifact":
        self.forced, self.reason = BLOCKED, reason
        return self

    @property
    def status(self) -> str:
        if any(not c.ok for c in self.checks):
            return FAIL
        return self.forced or PASS


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def rel(path: pathlib.Path) -> str:
    try:
        return str(path.resolve().relative_to(ROOT))
    except ValueError:
        return str(path)


def workspace_version() -> str:
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    return manifest["workspace"]["package"]["version"]


class Host:
    """Command runner + receipt sink for one invocation."""

    def __init__(self, args):
        self.args = args
        self.cargo = [args.cargo_wrapper, "cargo"] if args.cargo_wrapper else ["cargo"]
        self.out = pathlib.Path(args.out).resolve() if args.out else None
        self.scratch = pathlib.Path(tempfile.mkdtemp(prefix="package-inspect-"))
        self.artifacts: list[Artifact] = []
        self.version = workspace_version()
        if self.out:
            (self.out / "logs").mkdir(parents=True, exist_ok=True)

    def add(self, art: Artifact) -> Artifact:
        self.artifacts.append(art)
        return art

    def run(self, label: str, cmd: list, cwd: pathlib.Path = ROOT, timeout: int = 3600):
        """Run a command, keep its full output as a log, return the process."""
        try:
            proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)
        except FileNotFoundError as err:
            proc = subprocess.CompletedProcess(cmd, 127, "", f"{err}\n")
        except subprocess.TimeoutExpired as err:
            proc = subprocess.CompletedProcess(cmd, 124, err.stdout or "", f"timed out after {timeout} s\n")
        if self.out:
            safe = re.sub(r"[^A-Za-z0-9._-]+", "-", label).strip("-")
            body = (
                f"$ (cd {rel(cwd)} && {shlex.join(str(c) for c in cmd)})\n"
                f"exit {proc.returncode}\n--- stdout\n{proc.stdout}\n--- stderr\n{proc.stderr}"
            )
            (self.out / "logs" / f"{safe}.log").write_text(body)
        return proc

    def cargo_cmd(self, *args: str) -> list:
        return [*self.cargo, *args]

    def target_dir(self) -> pathlib.Path:
        proc = self.run("cargo-metadata", self.cargo_cmd("metadata", "--format-version", "1", "--no-deps"))
        if proc.returncode != 0:
            raise RuntimeError(f"cargo metadata failed: {tail(proc)}")
        return pathlib.Path(json.loads(proc.stdout)["target_directory"])


def tail(proc, lines: int = 12) -> str:
    text = (proc.stderr or "") + (proc.stdout or "")
    return " ⏎ ".join(text.strip().splitlines()[-lines:])


def which(tool: str):
    return shutil.which(tool)


# ---------------------------------------------------------------- archives


def read_tar(path: pathlib.Path):
    """Regular files of a tar(.gz) archive → ({path below the top dir: bytes}, top dir)."""
    files, tops = {}, set()
    with tarfile.open(path) as archive:
        for member in archive.getmembers():
            if not member.isfile():
                continue
            top, _, inner = member.name.partition("/")
            tops.add(top)
            handle = archive.extractfile(member)
            files[inner] = handle.read() if handle else b""
    return files, (tops.pop() if len(tops) == 1 else "|".join(sorted(tops)))


def read_zip(path: pathlib.Path) -> dict:
    with zipfile.ZipFile(path) as archive:
        return {name: archive.read(name) for name in archive.namelist() if not name.endswith("/")}


def describe(art: Artifact, path: pathlib.Path, names) -> None:
    data = path.read_bytes()
    art.path, art.sha256, art.size = rel(path), sha256(data), len(data)
    art.listing = sorted(names)


def check_license(art: Artifact, data, where: str) -> None:
    want = sha256(LICENSE.read_bytes())
    if data is None:
        art.check("LICENSE present", False, f"no {where} in the archive")
        return
    got = sha256(data)
    art.check(
        "LICENSE is the AGPL text, byte-identical to the root LICENSE",
        got == want,
        f"{where} sha256 {got[:16]}…" + ("" if got == want else f" ≠ root {want[:16]}…"),
    )


def check_stray(art: Artifact, names) -> None:
    hits = [f"{name} ({why})" for name in names for pattern, why in STRAY if pattern.search(name)]
    art.check("no private or stray files", not hits, "; ".join(hits) if hits else "")


_TRACKED: dict = {}


def tracked(directory: str):
    """Files git tracks under DIRECTORY, relative to it; None when git cannot say."""
    if directory not in _TRACKED:
        proc = subprocess.run(["git", "ls-files", "-z", "--", directory], cwd=ROOT, capture_output=True)
        prefix = directory.rstrip("/") + "/"
        _TRACKED[directory] = None if proc.returncode else {
            name[len(prefix):] for name in proc.stdout.decode().split("\0") if name.startswith(prefix)}
    return _TRACKED[directory]


def check_tracked(art: Artifact, names, sources: dict, generated=frozenset()) -> None:
    """Every entry is a file git tracks in the directory it was packed from, or
    one the packager generates: an untracked file (picked up through
    --allow-dirty, or left in a build directory) never ships.

    SOURCES maps an archive prefix to the repository directory packed under it.
    """
    allowed = set(generated)
    for prefix, directory in sources.items():
        files = tracked(directory)
        if files is None:
            art.check("every file is tracked by git or generated by the packager", False,
                      f"git ls-files {directory} failed")
            return
        allowed |= {prefix + name for name in files}
    extra = sorted(set(names) - allowed)
    art.check("every file is tracked by git or generated by the packager", not extra,
              f"not tracked by git: {', '.join(extra)}" if extra else "")


def glob_regex(pattern: str) -> re.Pattern:
    """A PEP 639 license-files glob (`*`, `?`, `**`, `[...]`) as a full-match regex."""
    out, i = [], 0
    while i < len(pattern):
        if pattern.startswith("**/", i):
            out.append("(?:.*/)?")
            i += 3
        elif pattern.startswith("**", i):
            out.append(".*")
            i += 2
        elif pattern[i] in "*?":
            out.append("[^/]*" if pattern[i] == "*" else "[^/]")
            i += 1
        elif pattern[i] == "[" and "]" in pattern[i + 1:]:
            end = pattern.index("]", i + 1)
            out.append(pattern[i:end + 1])
            i = end + 1
        else:
            out.append(re.escape(pattern[i]))
            i += 1
    return re.compile("".join(out))


def check_exact_set(art: Artifact, names, expected: frozenset, what: str) -> None:
    names = set(names)
    extra, missing = sorted(names - expected), sorted(expected - names)
    detail = "; ".join(filter(None, [
        f"unexpected: {', '.join(extra)}" if extra else "",
        f"missing: {', '.join(missing)}" if missing else "",
    ]))
    art.check(f"file set is exactly the {what}", not extra and not missing, detail)


def check_equal_file(art: Artifact, name: str, data, source: pathlib.Path) -> None:
    want = source.read_bytes()
    art.check(f"{name} is {rel(source)} verbatim", data == want,
              "" if data == want else f"differs from {rel(source)}")


# ---------------------------------------------------------------- crates


def inspect_crate(host: Host, path: pathlib.Path, notes=()) -> Artifact:
    files, top = read_tar(path)
    art = host.add(Artifact(f"crate {top}", "crate"))
    art.notes.extend(notes)
    describe(art, path, files)
    manifest = tomllib.loads(files.get("Cargo.toml", b"").decode() or "")
    package = manifest.get("package", {})
    name = package.get("name") or top.rsplit("-", 1)[0]
    art.check("archive root is <name>-<version>", top == f"{package.get('name')}-{package.get('version')}", top)
    art.check(f"version = {host.version} (workspace)", package.get("version") == host.version,
              f"Cargo.toml version {package.get('version')}")
    art.check(f"license = {SPDX}", package.get("license") == SPDX, f"{package.get('license')}")
    for required in ("Cargo.toml", "Cargo.toml.orig", "README.md"):
        art.check(f"{required} present", required in files)
    entry = "src/main.rs" if name.endswith("-cli") else "src/lib.rs"
    art.check(f"{entry} present", entry in files)
    if name.endswith("-cli"):
        dep = manifest.get("dependencies", {}).get("qrcode-ai-scanner", {})
        art.check(f"core dependency pinned at {host.version}", dep.get("version") == host.version,
                  f"qrcode-ai-scanner version {dep.get('version')!r}")
    readme = ROOT / "crates" / name / "README.md"
    if readme.is_file():
        check_equal_file(art, "README.md", files.get("README.md"), readme)
    check_license(art, files.get("LICENSE"), "LICENSE")
    check_tracked(art, files, {"": f"crates/{name}"}, CARGO_GENERATED)
    check_stray(art, files)
    # Profiles never ride along from the workspace root: what `cargo install`
    # builds is whatever this manifest itself says.
    rxing = manifest.get("profile", {}).get("release", {}).get("package", {}).get("rxing", {})
    if name.endswith("-cli"):
        art.notes.append(
            "packaged manifest carries rxing overflow-checks"
            if rxing.get("overflow-checks") is True
            else "packaged manifest has no [profile]: `cargo install` builds rxing without overflow-checks"
        )
    return art


def cmd_crates(host: Host) -> None:
    args = ["package", "--locked", *(f"-p{c}" for c in PUBLISHED_CRATES)]
    if not host.args.verify:
        args.append("--no-verify")
    if host.args.allow_dirty:
        args.append("--allow-dirty")
    proc = host.run("cargo-package", host.cargo_cmd(*args))
    try:
        target = host.target_dir()
    except RuntimeError as err:
        host.add(Artifact("crates", "crate")).check("cargo metadata", False, str(err))
        return
    mode = "packaged and verified (each crate built from its own archive)" if host.args.verify \
        else "packaged with --no-verify (crates-publish.yml verifies)"
    for crate in PUBLISHED_CRATES:
        path = target / "package" / f"{crate}-{host.version}.crate"
        if proc.returncode != 0 or not path.is_file():
            host.add(Artifact(f"crate {crate}-{host.version}", "crate")).check(
                "cargo package", False, f"exit {proc.returncode}: {tail(proc)}")
            continue
        inspect_crate(host, path, notes=[mode])


# ---------------------------------------------------------------- npm


def npm_pack(host: Host, label: str, pkg_dir: pathlib.Path):
    """`npm pack` into scratch, no lifecycle scripts → (tarball path, error)."""
    if not which("npm"):
        return None, "npm not on PATH"
    # Under --out the tarball stays next to the receipt that describes it.
    dest = (host.out / "archives" if host.out else host.scratch) / label
    dest.mkdir(parents=True, exist_ok=True)
    proc = host.run(f"npm-pack-{label}", ["npm", "pack", "--json", "--ignore-scripts",
                                          "--pack-destination", str(dest)], cwd=pkg_dir)
    if proc.returncode != 0:
        return None, f"npm pack exit {proc.returncode}: {tail(proc)}"
    try:
        meta = json.loads(proc.stdout)[0]
    except (ValueError, IndexError, KeyError) as err:
        return None, f"npm pack printed no JSON ({err})"
    return dest / meta["filename"], ""


def inspect_npm(host: Host, path: pathlib.Path, notes=()) -> Artifact:
    files, _top = read_tar(path)
    pkg = json.loads(files.get("package.json", b"{}"))
    name = pkg.get("name", "?")
    art = host.add(Artifact(f"npm {name}@{pkg.get('version')}", "npm"))
    art.notes.extend(notes)
    art.package = {key: pkg.get(key) for key in ("name", "version", "optionalDependencies")}
    describe(art, path, files)
    art.check(f"version = {host.version} (workspace)", pkg.get("version") == host.version,
              f"package.json version {pkg.get('version')}")
    art.check(f"license = {SPDX}", pkg.get("license") == SPDX, f"{pkg.get('license')}")
    art.check("repository is this repo (trusted publishing matches on it)",
              "github.com/supernovae-st/qrcode-ai-scanner" in json.dumps(pkg.get("repository", "")),
              json.dumps(pkg.get("repository")))
    check_license(art, files.get("LICENSE"), "package/LICENSE")
    check_stray(art, files)
    if name == NODE_NAME:
        check_exact_set(art, files, NODE_FILES, "main-package allowlist")
        native = files.get("native.js", b"").decode()
        literal = LOADER_LITERAL.search(native)
        art.check("loader enforces the package's own version (no literal)",
                  not literal and LOADER_RUNTIME_READ in native,
                  f"literal {literal.group(0)!r}" if literal else f"reads {LOADER_RUNTIME_READ}")
        check_equal_file(art, "report-types.d.ts", files.get("report-types.d.ts"), CANON_TYPES)
        check_equal_file(art, "README.md", files.get("README.md"), NODE_DIR / "README.md")
        for dts in ("index.d.ts", "native.d.ts"):
            check_equal_file(art, dts, files.get(dts), NODE_DIR / dts)
        # `napi pre-publish` pins every napi target at the package version: the
        # loader can only find a binary through those pins. The committed
        # manifest carries none (a dev build); a release set must carry all.
        optional = pkg.get("optionalDependencies")
        if optional is not None or host.args.release_set:
            check_pins(art, optional or {}, pkg)
    elif name == WASM_NAME:
        check_exact_set(art, files, WASM_FILES, "wasm-pack output + patch allowlist")
        check_equal_file(art, "report-types.d.ts", files.get("report-types.d.ts"), CANON_TYPES)
        dts = files.get("qrcode-ai-scanner.d.ts", b"").decode()
        retyped = all(
            re.search(rf"export function {fn}\([^)]*\): ScanReport;", dts) for fn in ("scan_image", "scan_frame")
        )
        art.check("qrcode-ai-scanner.d.ts re-exports report-types and returns ScanReport",
                  'export * from "./report-types"' in dts and retyped)
        check_equal_file(art, "README.md", files.get("README.md"), WASM_DIR / "README.md")
    elif name.startswith(NODE_NAME + "-"):
        binaries = sorted(f for f in files if f.endswith(".node"))
        art.check("exactly one .node binary, and it is `main`",
                  len(binaries) == 1 and binaries[0] == pkg.get("main"), f"{binaries} main={pkg.get('main')}")
        expected = frozenset({"package.json", "LICENSE", *binaries, *(["README.md"] if "README.md" in files else [])})
        check_exact_set(art, files, expected, "platform-package set (package.json · README · LICENSE · binary)")
        art.check("os + cpu declared", bool(pkg.get("os")) and bool(pkg.get("cpu")),
                  f"os={pkg.get('os')} cpu={pkg.get('cpu')}")
    else:
        art.check("known package name", False, name)
    return art


def check_pins(art: Artifact, optional: dict, pkg: dict) -> None:
    version = pkg.get("version")
    targets = pkg.get("napi", {}).get("targets") or []
    want = {f"{NODE_NAME}-{napi_suffix(triple)}": version for triple in targets}
    problems = [f"missing {dep}" for dep in sorted(set(want) - set(optional))]
    problems += [f"unexpected {dep}" for dep in sorted(set(optional) - set(want))]
    problems += [f"{dep} pinned at {optional[dep]}, not {version}" for dep in sorted(set(want) & set(optional))
                 if optional[dep] != version]
    art.check(f"optionalDependencies pin every napi target ({len(want)}) at {version}",
              bool(want) and not problems, "; ".join(problems) or f"{len(want)} pins")


def check_release_set(host: Host) -> None:
    """--release-set: one main package, and exactly one platform package per pin."""
    npm = [a for a in host.artifacts if a.kind == "npm" and a.package.get("name")]
    mains = [a for a in npm if a.package["name"] == NODE_NAME]
    art = host.add(Artifact("npm release set", "npm"))
    if not art.check("exactly one main package", len(mains) == 1, f"{len(mains)} found"):
        return
    pins = mains[0].package.get("optionalDependencies") or {}
    platforms = {a.package["name"]: a.package.get("version") for a in npm
                 if a.package["name"].startswith(NODE_NAME + "-") and a.package["name"] != WASM_NAME}
    problems = [f"no {dep}@{version} platform package" for dep, version in sorted(pins.items())
                if platforms.get(dep) != version]
    problems += [f"{dep} is not pinned by the main package" for dep in sorted(set(platforms) - set(pins))]
    art.listing = sorted(f"{dep}@{version}" for dep, version in platforms.items())
    art.check(f"one platform package per pin ({len(pins)}), at the pinned version", bool(pins) and not problems,
              "; ".join(problems))


NAPI_SUFFIX = {
    ("darwin", "arm64"): "darwin-arm64", ("darwin", "x86_64"): "darwin-x64",
    ("linux", "x86_64"): "linux-x64-gnu", ("linux", "aarch64"): "linux-arm64-gnu",
    ("win32", "AMD64"): "win32-x64-msvc",
}

# napi-rs's platformArchABI, ported as-is from @napi-rs/cli 3.7.3
# (cli/src/utils/target.ts parseTriple): the suffix of each platform package
# that `napi pre-publish` pins in the main package's optionalDependencies.
_NAPI_CPU = {"x86_64": "x64", "aarch64": "arm64", "i686": "ia32", "armv7": "arm",
             "loongarch64": "loong64", "riscv64gc": "riscv64", "powerpc64le": "ppc64"}
_NAPI_SYS = {"linux": "linux", "freebsd": "freebsd", "darwin": "darwin", "windows": "win32",
             "ohos": "openharmony"}


def napi_suffix(triple: str) -> str:
    if triple in ("wasm32-wasi", "wasm32-wasi-preview1-threads") or triple.startswith("wasm32-wasip"):
        return "wasm32-wasi"
    parts = (f"{triple[:-4]}-eabi" if triple.endswith("eabi") else triple).split("-")
    if len(parts) == 2:
        cpu, system, abi = parts[0], parts[1], None
    else:
        cpu, system, abi = parts[0], parts[2], parts[3] if len(parts) > 3 else None
    if abi in ("android", "ohos"):
        system, abi = abi, None
    platform_name, arch = _NAPI_SYS.get(system, system), _NAPI_CPU.get(cpu, cpu)
    return f"{platform_name}-{arch}-{abi}" if abi else f"{platform_name}-{arch}"


def napi_suffixes() -> list:
    """The platform-package suffix of every target in the node package.json."""
    targets = json.loads((NODE_DIR / "package.json").read_text())["napi"]["targets"]
    return [napi_suffix(triple) for triple in targets]


def emulated_platform_dir(host: Host) -> pathlib.Path:
    """A platform package in the shape `napi create-npm-dirs` writes, + LICENSE.

    Proves npm's always-included LICENSE survives the platform package's
    `files` allowlist; CI (npm-publish.yml pack-native) inspects the real dirs.
    """
    import platform
    suffix = NAPI_SUFFIX.get((sys.platform, platform.machine()), "linux-x64-gnu")
    binary = f"qrcode-ai-scanner.{suffix}.node"
    node_pkg = json.loads((NODE_DIR / "package.json").read_text())
    os_name, cpu = suffix.split("-")[:2]
    pdir = host.scratch / "emulated-platform"
    pdir.mkdir(parents=True, exist_ok=True)
    (pdir / "package.json").write_text(json.dumps({
        "name": f"{NODE_NAME}-{suffix}",
        "version": node_pkg["version"],
        "cpu": [cpu],
        "main": binary,
        "files": [binary],
        "description": node_pkg.get("description"),
        "license": node_pkg.get("license"),
        "engines": node_pkg.get("engines"),
        "repository": node_pkg.get("repository"),
        "os": [os_name],
    }, indent=2))
    (pdir / "README.md").write_text(f"# `{NODE_NAME}-{suffix}`\n")
    shutil.copyfile(NODE_DIR / "LICENSE", pdir / "LICENSE")
    built = NODE_DIR / binary
    if built.is_file():
        shutil.copyfile(built, pdir / binary)
    else:
        (pdir / binary).write_bytes(b"placeholder: no local napi build\n")
    return pdir


def cmd_node(host: Host) -> None:
    tarball, err = npm_pack(host, "node-main", NODE_DIR)
    if tarball is None:
        host.add(Artifact(f"npm {NODE_NAME}", "npm")).unavailable(err)
        return
    inspect_npm(host, tarball, notes=["packed from the source tree (--ignore-scripts: the committed files)"])
    platform_root = pathlib.Path(host.args.platform_dirs).resolve() if host.args.platform_dirs else None
    if platform_root:
        dirs = sorted(d for d in platform_root.iterdir() if (d / "package.json").is_file())
        if not dirs:
            host.add(Artifact("npm platform packages", "npm")).check(
                "platform package dirs", False, f"none under {rel(platform_root)}")
        for pdir in dirs:
            tarball, err = npm_pack(host, f"node-{pdir.name}", pdir)
            if tarball is None:
                host.add(Artifact(f"npm platform {pdir.name}", "npm")).check("npm pack", False, err)
            else:
                inspect_npm(host, tarball, notes=["napi create-npm-dirs + napi artifacts output"])
        if host.args.release_set:
            check_release_set(host)
        return
    if host.args.release_set:
        host.add(Artifact("npm release set", "npm")).check("--platform-dirs given", False,
                                                           "a release set needs its platform packages")
        return
    tarball, err = npm_pack(host, "node-platform-emulated", emulated_platform_dir(host))
    if tarball is None:
        host.add(Artifact("npm platform package (emulated)", "npm")).unavailable(err)
        return
    inspect_npm(host, tarball, notes=[
        "EMULATED: the napi create-npm-dirs package shape with LICENSE copied in, packed by the real npm; "
        "the real per-target dirs are inspected in npm-publish.yml (pack-native)",
    ])


def wasm_opt_version():
    if not which("wasm-opt"):
        return None
    proc = subprocess.run(["wasm-opt", "--version"], capture_output=True, text=True)
    found = re.search(r"version (\d+)", proc.stdout)
    return int(found.group(1)) if found else None


def cmd_wasm(host: Host) -> None:
    pkg = WASM_DIR / "pkg"
    note = "pkg/ as found (--no-build)"
    if not host.args.no_build:
        missing = [t for t in ("wasm-pack", "node") if not which(t)]
        opt = wasm_opt_version()
        if missing or opt is None or opt < 130:
            reason = ", ".join([*(f"{t} not on PATH" for t in missing),
                                *([] if opt and opt >= 130 else [f"wasm-opt {opt or 'absent'} (needs binaryen >= 130)"])])
            host.add(Artifact(f"npm {WASM_NAME}", "npm")).unavailable(reason)
            return
        proc = host.run("build-wasm", ["bash", str(ROOT / "scripts" / "build-wasm.sh")])
        if proc.returncode != 0:
            host.add(Artifact(f"npm {WASM_NAME}", "npm")).check(
                "scripts/build-wasm.sh (build · wasm-opt · patch · smoke)", False, f"exit {proc.returncode}: {tail(proc)}")
            return
        note = "pkg/ built by scripts/build-wasm.sh (wasm-pack · wasm-opt · patch · test.mjs)"
    if not (pkg / "package.json").is_file():
        host.add(Artifact(f"npm {WASM_NAME}", "npm")).unavailable("no crates/qrcode-ai-scanner-wasm/pkg/ to pack")
        return
    tarball, err = npm_pack(host, "wasm", pkg)
    if tarball is None:
        host.add(Artifact(f"npm {WASM_NAME}", "npm")).unavailable(err)
        return
    inspect_npm(host, tarball, notes=[note])


# ---------------------------------------------------------------- python


def metadata(data: bytes):
    return email.parser.BytesParser().parsebytes(data)


def check_python_metadata(art: Artifact, host: Host, meta) -> None:
    art.check(f"Name = {PYPI_NAME}", meta.get("Name") == PYPI_NAME, f"{meta.get('Name')}")
    art.check(f"Version = {host.version} (workspace)", meta.get("Version") == host.version, f"{meta.get('Version')}")
    expression = meta.get("License-Expression") or meta.get("License")
    art.check(f"License-Expression = {SPDX}", expression == SPDX, f"{expression}")
    files = meta.get_all("License-File") or []
    art.check("License-File names LICENSE", "LICENSE" in files, f"License-File: {files}")


def inspect_wheel(host: Host, path: pathlib.Path, notes=()) -> Artifact:
    files = read_zip(path)
    art = host.add(Artifact(f"wheel {path.name}", "wheel"))
    art.notes.extend(notes)
    describe(art, path, files)
    dist_info = sorted({n.split("/", 1)[0] for n in files if re.match(r"[^/]+\.dist-info/METADATA$", n)})
    if not art.check("one .dist-info/METADATA", len(dist_info) == 1, ", ".join(dist_info)):
        return art
    info = dist_info[0]
    meta = metadata(files[f"{info}/METADATA"])
    check_python_metadata(art, host, meta)
    # PEP 639: each License-File value names a file under .dist-info/licenses/.
    for value in meta.get_all("License-File") or ["LICENSE"]:
        check_license(art, files.get(f"{info}/licenses/{value}"), f"{info}/licenses/{value} (License-File {value})")
    for typed in ("qrcode_ai_scanner/__init__.pyi", "qrcode_ai_scanner/py.typed"):
        art.check(f"{typed} present (type stubs)", typed in files)
    modules = [n for n in files if re.match(r"qrcode_ai_scanner/qrcode_ai_scanner\.[^/]*(so|pyd)$", n)]
    art.check("exactly one compiled extension module", len(modules) == 1, ", ".join(modules))
    # The whole wheel, exactly: the dist-info maturin writes, the license
    # files, the SBOM, and the package (init · stubs · marker · extension).
    exact = {f"{info}/{n}" for n in ("METADATA", "WHEEL", "RECORD")} | {
        f"{info}/licenses/{v}" for v in meta.get_all("License-File") or []} | {
        f"qrcode_ai_scanner/{n}" for n in ("__init__.py", "__init__.pyi", "py.typed")}
    patterns = (re.compile(rf"{re.escape(info)}/sboms/[^/]+\.cyclonedx\.json"),
                re.compile(r"qrcode_ai_scanner/qrcode_ai_scanner(\.[^/]+)?\.(so|pyd)"))
    extra = sorted(n for n in files if n not in exact and not any(p.fullmatch(n) for p in patterns))
    art.check("file set is exactly the wheel allowlist (dist-info · licenses · sbom · package)", not extra,
              f"unexpected: {', '.join(extra)}" if extra else "")
    check_stray(art, files)
    return art


def inspect_sdist(host: Host, path: pathlib.Path, notes=()) -> Artifact:
    files, top = read_tar(path)
    art = host.add(Artifact(f"sdist {path.name}", "sdist"))
    art.notes.extend(notes)
    describe(art, path, files)
    art.check(f"archive root is qrcode_ai_scanner-{host.version}", top == f"qrcode_ai_scanner-{host.version}", top)
    meta = metadata(files["PKG-INFO"]) if "PKG-INFO" in files else None
    if meta is not None:
        check_python_metadata(art, host, meta)
    else:
        art.check("PKG-INFO present", False)
    for required in ("pyproject.toml", "README.md"):
        art.check(f"{required} present", required in files)
    # PEP 639: License-File values and the pyproject's license-files globs are
    # relative to the sdist root, where the rewritten pyproject.toml sits. A
    # copy only inside the binding crate directory does not satisfy them, and a
    # build from this sdist would find no license file.
    for value in (meta.get_all("License-File") or []) if meta is not None else []:
        check_license(art, files.get(value), f"License-File {value} at the sdist root")
    project = tomllib.loads(files.get("pyproject.toml", b"").decode() or "").get("project", {})
    globs = project.get("license-files")
    art.check("pyproject.toml declares license-files (PEP 639)", isinstance(globs, list) and bool(globs), f"{globs}")
    licensed = set()
    for pattern in globs if isinstance(globs, list) else []:
        matched = sorted(n for n in files if glob_regex(pattern).fullmatch(n))
        if art.check(f"license-files {pattern!r} matches a file at the sdist root", bool(matched),
                     ", ".join(matched)):
            for name in matched:
                check_license(art, files[name], f"{name} (license-files {pattern!r})")
        licensed |= set(matched)
    check_license(art, files.get("qrcode-ai-scanner/LICENSE"), "qrcode-ai-scanner/LICENSE (vendored core)")
    check_tracked(art, files, SDIST_CRATES, MATURIN_SDIST_ROOT | licensed | {
        prefix + name for prefix in SDIST_CRATES for name in CARGO_GENERATED})
    py_manifest = tomllib.loads(files.get("qrcode-ai-scanner-py/Cargo.toml", b"").decode() or "")
    rxing = py_manifest.get("profile", {}).get("release", {}).get("package", {}).get("rxing", {})
    art.check("the shipped binding manifest keeps rxing overflow-checks (an sdist build is its own root)",
              rxing.get("overflow-checks") is True, f"profile.release.package.rxing = {rxing}")
    check_stray(art, files)
    return art


def cmd_python(host: Host) -> None:
    maturin = which("maturin")
    if not maturin:
        host.add(Artifact(f"wheel {PYPI_NAME} {host.version}", "wheel")).unavailable("maturin not on PATH")
        host.add(Artifact(f"sdist {PYPI_NAME} {host.version}", "sdist")).unavailable("maturin not on PATH")
        return
    dest = (host.out / "archives" if host.out else host.scratch) / "python"
    proc = host.run("maturin-sdist", [maturin, "sdist", "--out", str(dest)], cwd=PY_DIR)
    sdists = sorted(dest.glob("*.tar.gz"))
    if proc.returncode != 0 or len(sdists) != 1:
        host.add(Artifact(f"sdist {PYPI_NAME}", "sdist")).check("maturin sdist", False, tail(proc))
    else:
        inspect_sdist(host, sdists[0], notes=["maturin sdist"])
    proc = host.run("maturin-build", [maturin, "build", "--release", "--locked", "--out", str(dest)], cwd=PY_DIR)
    wheels = sorted(dest.glob("*.whl"))
    if proc.returncode != 0 or len(wheels) != 1:
        host.add(Artifact(f"wheel {PYPI_NAME}", "wheel")).check("maturin build --release --locked", False, tail(proc))
    else:
        inspect_wheel(host, wheels[0], notes=["maturin build --release --locked (host wheel)"])


# ---------------------------------------------------------------- flutter


def parse_pub_tree(output: str) -> list:
    """File leaves of the tree `pub publish --dry-run` prints.

    Current pub draws `├── name (1 KB)` rows with `│   ` indents; older pub
    drew `|-- name` / `'-- name` with `|   `.
    """
    rows = []
    for line in output.splitlines():
        found = re.match(r"^((?:[│| ]   )*)(?:[├└]──|[|'`]--) (.+)$", line)
        if found:
            name = re.sub(r" \(<?[\d.]+ [KMG]?B\)$", "", found.group(2).strip())
            rows.append((len(found.group(1)) // 4, name))
    leaves, stack = [], []
    for i, (depth, name) in enumerate(rows):
        del stack[depth:]
        stack.append(name)
        if i + 1 == len(rows) or rows[i + 1][0] <= depth:
            leaves.append("/".join(stack))
    return leaves


def cmd_flutter(host: Host) -> None:
    pubspec = (FLUTTER_DIR / "pubspec.yaml").read_text()
    found = re.search(r"^version:\s*(\S+)", pubspec, re.MULTILINE)
    version = found.group(1) if found else None
    art = host.add(Artifact(f"pub qrcode_ai_scanner {version}", "pub"))
    art.check(f"pubspec version = {host.version} (workspace)", version == host.version, f"{version}")
    binary = host.args.flutter or which("flutter") or which("dart")
    listing, source = None, ""
    if binary:
        proc = host.run("pub-publish-dry-run", [binary, "pub", "publish", "--dry-run"], cwd=FLUTTER_DIR, timeout=900)
        leaves = parse_pub_tree(proc.stdout)
        if leaves:
            listing, source = leaves, f"{pathlib.Path(binary).name} pub publish --dry-run (exit {proc.returncode})"
            warnings = re.search(r"Package has (\d+) warnings?", proc.stdout + proc.stderr)
            art.notes.append(f"pub validator: {warnings.group(0) if warnings else 'no warning count printed'}")
        else:
            art.notes.append(f"pub publish --dry-run printed no file tree (exit {proc.returncode}): {tail(proc, 4)}")
    if listing is None:
        proc = host.run("git-ls-files-flutter", ["git", "ls-files", "--cached", "--others", "--exclude-standard",
                                                 "--", "."], cwd=FLUTTER_DIR)
        listing = proc.stdout.split()
        source = "git ls-files approximation (no Flutter/Dart SDK): pub honours the same .gitignore rules"
    art.listing = sorted(listing)
    art.path = rel(FLUTTER_DIR)
    art.notes.append(f"listing: {source}")
    art.check("LICENSE at the package root", "LICENSE" in listing)
    check_license(art, (FLUTTER_DIR / "LICENSE").read_bytes() if (FLUTTER_DIR / "LICENSE").is_file() else None,
                  "bindings/flutter/LICENSE")
    for required in ("README.md", "CHANGELOG.md", "pubspec.yaml", "rust/Cargo.toml"):
        art.check(f"{required} in the package", required in listing)
    check_stray(art, listing)
    crate = tomllib.loads((FLUTTER_DIR / "rust" / "Cargo.toml").read_text())
    rxing = crate.get("profile", {}).get("release", {}).get("package", {}).get("rxing", {})
    art.check("rust/Cargo.toml keeps rxing overflow-checks (cargokit builds it as its own root)",
              rxing.get("overflow-checks") is True, f"profile.release.package.rxing = {rxing}")
    escaping = []
    for dep_name, dep in crate.get("dependencies", {}).items():
        if isinstance(dep, dict) and "path" in dep:
            target = (FLUTTER_DIR / "rust" / dep["path"]).resolve()
            if FLUTTER_DIR.resolve() not in target.parents and target != FLUTTER_DIR.resolve():
                escaping.append(f"{dep_name} → {dep['path']}")
    if escaping:
        art.blocked("the Rust crate's path dependency leaves the package, so the published package cannot "
                    f"build its core ({'; '.join(escaping)}); pub.dev's first publish is also manual")


# ---------------------------------------------------------------- licenses · versions


def cmd_licenses(host: Host) -> None:
    art = host.add(Artifact("LICENSE copies", "licenses"))
    want = LICENSE.read_bytes()
    art.path, art.sha256, art.size = "LICENSE", sha256(want), len(want)
    for copy in LICENSE_COPIES:
        path = ROOT / copy
        same = path.is_file() and path.read_bytes() == want
        if not same and host.args.sync:
            path.write_bytes(want)
            same = True
            art.notes.append(f"synced {copy}")
        art.check(f"{copy} byte-identical to LICENSE", same, "" if same else "missing or different (--sync)")
    art.listing = list(LICENSE_COPIES)
    # A Windows checkout (core.autocrlf=true, Git for Windows' default) would
    # write every copy with CRLF, and the win_amd64 wheel would ship that text,
    # which no longer matches the LF root. .gitattributes pins them to LF.
    paths = ["LICENSE", *LICENSE_COPIES]
    proc = subprocess.run(["git", "check-attr", "text", "eol", "--", *paths],
                          cwd=ROOT, capture_output=True, text=True)
    attrs: dict = {}
    for line in proc.stdout.splitlines():
        path, attr, value = line.rsplit(": ", 2)
        attrs.setdefault(path, {})[attr] = value
    loose = [p for p in paths if attrs.get(p) != {"text": "set", "eol": "lf"}]
    art.check("every copy checks out with LF on every platform (.gitattributes: LICENSE text eol=lf)",
              proc.returncode == 0 and not loose,
              f"git check-attr exit {proc.returncode}: {proc.stderr.strip()}" if proc.returncode
              else "; ".join(f"{p}: {attrs.get(p)}" for p in loose))


def _toml_version(path: str, *keys: str):
    node = tomllib.loads((ROOT / path).read_text())
    for key in keys:
        node = node.get(key, {}) if isinstance(node, dict) else {}
    return node if isinstance(node, str) else None


def _regex_version(path: str, pattern: str):
    found = re.search(pattern, (ROOT / path).read_text(), re.MULTILINE)
    return found.group(1) if found else None


# The manifests each publish job ships from (xtask sync-version rewrites them;
# mobile.yml checks them on every push).
SURFACES = (
    ("workspace", "Cargo.toml", lambda p: _toml_version(p, "workspace", "package", "version")),
    ("cli core pin", "crates/qrcode-ai-scanner-cli/Cargo.toml",
     lambda p: _toml_version(p, "dependencies", "qrcode-ai-scanner", "version")),
    ("node package.json", "crates/qrcode-ai-scanner-node/package.json",
     lambda p: json.loads((ROOT / p).read_text()).get("version")),
    ("python crate", "crates/qrcode-ai-scanner-py/Cargo.toml", lambda p: _toml_version(p, "package", "version")),
    ("uniffi crate", "crates/qrcode-ai-scanner-uniffi/Cargo.toml", lambda p: _toml_version(p, "package", "version")),
    ("flutter pubspec", "bindings/flutter/pubspec.yaml", lambda p: _regex_version(p, r"^version:\s*(\S+)")),
    ("flutter rust crate", "bindings/flutter/rust/Cargo.toml", lambda p: _toml_version(p, "package", "version")),
    ("kotlin module", "bindings/kotlin/qrcodeaiscanner/build.gradle.kts",
     lambda p: _regex_version(p, r'^version\s*=\s*"([^"]+)"')),
)


def cmd_versions(host: Host) -> None:
    art = host.add(Artifact(f"version surfaces {host.version}", "versions"))
    for label, path, read in SURFACES:
        found = read(path)
        art.check(f"{label} = {host.version}", found == host.version, f"{path}: {found}")
    tag = host.args.tag
    if tag:
        tag = tag.removeprefix("refs/tags/")
        art.check(f"tag {tag} names the workspace version", tag == f"v{host.version}", f"workspace {host.version}")
        dated = re.compile(rf"^## {re.escape(host.version)} — \d{{4}}-\d{{2}}-\d{{2}}", re.MULTILINE)
        art.check(f"CHANGELOG.md has a dated '## {host.version} — YYYY-MM-DD' section",
                  bool(dated.search((ROOT / "CHANGELOG.md").read_text())))


# ---------------------------------------------------------------- profiles

# Every cargo build root that compiles rxing into something we ship. Cargo
# reads [profile] from the root manifest only, so each excluded crate must
# restate the workspace's release profile itself.
PROFILE_ROOTS = (
    ("workspace · node + cli", ["--manifest-path", "Cargo.toml",
                                "-p", "qrcode-ai-scanner-node", "-p", "qrcode-ai-scanner-cli"]),
    ("workspace · wasm32", ["--manifest-path", "Cargo.toml", "-p", "qrcode-ai-scanner-wasm",
                            "--target", "wasm32-unknown-unknown"]),
    ("python (maturin)", ["--manifest-path", "crates/qrcode-ai-scanner-py/Cargo.toml"]),
    ("uniffi (kotlin · swift)", ["--manifest-path", "crates/qrcode-ai-scanner-uniffi/Cargo.toml", "--lib"]),
    ("flutter (cargokit)", ["--manifest-path", "bindings/flutter/rust/Cargo.toml"]),
)
PROFILE_FIELDS = ("opt_level", "lto", "codegen_units", "debug_assertions", "overflow_checks", "panic", "strip")


def unit_name(unit: dict) -> str:
    pkg_id = unit.get("pkg_id", "")
    if "#" in pkg_id:  # registry+https://…#rxing@0.9.1 · path+file:///…#0.9.0
        tail_part = pkg_id.rsplit("#", 1)[1]
        name = tail_part.split("@", 1)[0]
        if re.fullmatch(r"\d+\.\d+\.\d+.*", name):  # path ids omit the name when it matches the dir
            name = pkg_id.rsplit("#", 1)[0].rstrip("/").rsplit("/", 1)[-1]
        return name
    return pkg_id.split(" ", 1)[0]


def profile_row(unit: dict) -> dict:
    prof = unit.get("profile", {})
    return {field: prof.get(field) for field in PROFILE_FIELDS}


def nightly_toolchain(host: Host):
    if host.args.nightly:
        return host.args.nightly
    if not which("rustup"):
        return None
    proc = subprocess.run(["rustup", "toolchain", "list"], capture_output=True, text=True)
    names = [line.split()[0] for line in proc.stdout.splitlines() if line.startswith("nightly")]
    return names[0] if names else None


def cmd_profiles(host: Host) -> None:
    nightly = nightly_toolchain(host)
    roots = [(label, ["--locked", *selector]) for label, selector in PROFILE_ROOTS]
    try:
        crate = host.target_dir() / "package" / f"qrcode-ai-scanner-cli-{host.version}.crate"
        if crate.is_file():
            with tarfile.open(crate) as archive:
                archive.extractall(host.scratch / "cli-crate", filter="data")
            packaged_cli = host.scratch / "cli-crate" / f"qrcode-ai-scanner-cli-{host.version}"
            # The packaged lock pins the core as packaged from this tree, which
            # the registry does not hold until release: resolve fresh instead
            # (profiles apply by package name, whatever version resolves).
            (packaged_cli / "Cargo.lock").unlink(missing_ok=True)
            roots.append(("cargo install qrcode-ai-scanner-cli (the packaged crate as its own root)",
                          ["--manifest-path", str(packaged_cli / "Cargo.toml")]))
    except RuntimeError:
        pass
    for label, selector in roots:
        art = host.add(Artifact(f"release profile · {label}", "profile"))
        if not nightly:
            art.unavailable("no nightly toolchain (`--unit-graph` is unstable)")
            continue
        proc = host.run(f"unit-graph-{label}", host.cargo_cmd(
            f"+{nightly}", "build", "--release", "--unit-graph", "-Z", "unstable-options", *selector))
        if proc.returncode != 0:
            art.check("cargo --unit-graph", False, f"exit {proc.returncode}: {tail(proc)}")
            continue
        graph = json.loads(proc.stdout)
        units = [u for u in graph.get("units", []) if u.get("mode") == "build"
                 and "custom-build" not in u.get("target", {}).get("kind", [])]
        roots_idx = graph.get("roots", [])
        root_units = [graph["units"][i] for i in roots_idx]
        rxing = [u for u in units if unit_name(u) == "rxing"]
        art.notes.append(f"via cargo +{nightly} build --release --unit-graph (no compilation)")
        rows = [("root " + unit_name(u), profile_row(u)) for u in root_units]
        rows += [("rxing", profile_row(u)) for u in rxing[:1]]
        art.listing = [f"{who}: " + ", ".join(f"{k}={v}" for k, v in row.items()) for who, row in rows]
        art.check("rxing compiled with overflow-checks", bool(rxing) and all(
            u["profile"].get("overflow_checks") is True for u in rxing),
            f"{len(rxing)} rxing unit(s): overflow_checks={[u['profile'].get('overflow_checks') for u in rxing]}")
        art.check("root units unwind on panic (the engine wrapper catches panics)",
                  all(u["profile"].get("panic") == "unwind" for u in root_units),
                  f"{[u['profile'].get('panic') for u in root_units]}")
        art.check("root units at opt-level 3", all(str(u["profile"].get("opt_level")) == "3" for u in root_units))


# ---------------------------------------------------------------- archive · all


def cmd_archive(host: Host) -> None:
    if not host.args.files:
        print("archive: give at least one file", file=sys.stderr)
        sys.exit(2)
    for name in host.args.files:
        # Unresolved: an archive outside the repo is reported as given, never as
        # the symlink-resolved path (which can name a private home directory).
        path = pathlib.Path(name)
        note = [f"given archive {name}"]
        if not path.is_file():
            host.add(Artifact(f"archive {name}", "archive")).check("file exists", False, name)
        elif path.name.endswith(".crate"):
            inspect_crate(host, path, notes=note)
        elif path.name.endswith(".whl"):
            inspect_wheel(host, path, notes=note)
        elif path.name.endswith(".tgz"):
            inspect_npm(host, path, notes=note)
        elif path.name.endswith(".tar.gz"):
            inspect_sdist(host, path, notes=note)
        else:
            host.add(Artifact(f"archive {name}", "archive")).check("known archive kind", False, path.suffix)
    if host.args.release_set:
        check_release_set(host)


def cmd_all(host: Host) -> None:
    for step in (cmd_licenses, cmd_versions, cmd_crates, cmd_node, cmd_wasm, cmd_python, cmd_flutter, cmd_profiles):
        try:
            step(host)
        except Exception as err:  # one broken target must not hide the others
            host.add(Artifact(step.__name__.removeprefix("cmd_"), "error")).check("ran", False, repr(err))


# ---------------------------------------------------------------- selftest

# One name per class of file the stray gates exist for. Each scenario plants
# one in an otherwise clean synthetic archive, which must then FAIL with that
# name in a failing check.
PLANTED = (
    ".netrc", "_netrc", ".pypirc", ".git-credentials", ".npmrc", ".env", ".ENV.production", ".envrc",
    "id_rsa", "id_ed25519", "deploy/ID_ECDSA", ".ssh/known_hosts", "release.jks", "upload.keystore",
    "AuthKey_ABC123.p8", "signing.pfx", "dev.p12", "server.pem", "secring.gpg", "Credentials.json",
    "secrets.toml", ".vscode/settings.json", ".idea/workspace.xml", ".claude/settings.local.json",
    ".cursor/rules.mdc", ".codex/config.toml", "notes.zip", "vendor.tar.gz", "nested.crate", "dist.whl",
    "plans/roadmap.md", "target/release/build.log", "node_modules/x/index.js", ".git/config", ".DS_Store",
    "lib.rs~",
)
REPO_URL = {"type": "git", "url": "https://github.com/supernovae-st/qrcode-ai-scanner"}


def _tar(path: pathlib.Path, top: str, files: dict) -> pathlib.Path:
    with tarfile.open(path, "w:gz") as archive:
        for name, data in sorted(files.items()):
            info = tarfile.TarInfo(f"{top}/{name}")
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
    return path


def _zip(path: pathlib.Path, files: dict) -> pathlib.Path:
    with zipfile.ZipFile(path, "w") as archive:
        for name, data in sorted(files.items()):
            archive.writestr(name, data)
    return path


def _clean_crate(version: str) -> dict:
    core = ROOT / "crates" / "qrcode-ai-scanner"
    manifest = f'[package]\nname = "qrcode-ai-scanner"\nversion = "{version}"\nlicense = "{SPDX}"\n'
    return {
        "Cargo.toml": manifest.encode(), "Cargo.toml.orig": (core / "Cargo.toml").read_bytes(),
        ".cargo_vcs_info.json": b'{"git": {"sha1": "0"}, "path_in_vcs": "crates/qrcode-ai-scanner"}\n',
        "Cargo.lock": b"version = 4\n", "src/lib.rs": b"//! synthetic\n",
        "README.md": (core / "README.md").read_bytes(), "LICENSE": LICENSE.read_bytes(),
    }


def _python_metadata(version: str) -> bytes:
    return (f"Metadata-Version: 2.4\nName: {PYPI_NAME}\nVersion: {version}\n"
            f"License-Expression: {SPDX}\nLicense-File: LICENSE\n").encode()


def _clean_wheel(version: str) -> dict:
    info = f"qrcode_ai_scanner-{version}.dist-info"
    return {
        f"{info}/METADATA": _python_metadata(version), f"{info}/WHEEL": b"Wheel-Version: 1.0\n",
        f"{info}/RECORD": b"", f"{info}/licenses/LICENSE": LICENSE.read_bytes(),
        f"{info}/sboms/qrcode-ai-scanner-py.cyclonedx.json": b"{}\n",
        "qrcode_ai_scanner/__init__.py": b"", "qrcode_ai_scanner/__init__.pyi": b"",
        "qrcode_ai_scanner/py.typed": b"", "qrcode_ai_scanner/qrcode_ai_scanner.abi3.so": b"\x7fELF",
    }


def _clean_sdist(version: str) -> dict:
    py, core = PY_DIR, ROOT / "crates" / "qrcode-ai-scanner"
    return {
        "PKG-INFO": _python_metadata(version), "pyproject.toml": (py / "pyproject.toml").read_bytes(),
        "README.md": (py / "README.md").read_bytes(), "LICENSE": LICENSE.read_bytes(),
        "qrcode-ai-scanner-py/Cargo.toml": (py / "Cargo.toml").read_bytes(),
        "qrcode-ai-scanner-py/src/lib.rs": b"", "qrcode-ai-scanner-py/LICENSE": LICENSE.read_bytes(),
        "qrcode-ai-scanner/Cargo.toml": (core / "Cargo.toml").read_bytes(),
        "qrcode-ai-scanner/src/lib.rs": b"", "qrcode-ai-scanner/README.md": (core / "README.md").read_bytes(),
        "qrcode-ai-scanner/LICENSE": LICENSE.read_bytes(),
    }


def _npm_main(version: str, optional=None) -> dict:
    pkg = json.loads((NODE_DIR / "package.json").read_text())
    if optional is not None:
        pkg["optionalDependencies"] = optional
    files = {name: (NODE_DIR / name).read_bytes() for name in ("README.md", "index.js", "index.d.ts",
                                                               "native.js", "native.d.ts")}
    return files | {"package.json": json.dumps(pkg, indent=2).encode(), "LICENSE": LICENSE.read_bytes(),
                    "report-types.d.ts": CANON_TYPES.read_bytes()}


def _npm_platform(version: str, suffix: str) -> dict:
    binary = f"qrcode-ai-scanner.{suffix}.node"
    os_name, cpu = suffix.split("-")[:2]
    pkg = {"name": f"{NODE_NAME}-{suffix}", "version": version, "os": [os_name], "cpu": [cpu],
           "main": binary, "files": [binary], "license": SPDX, "repository": REPO_URL}
    return {"package.json": json.dumps(pkg).encode(), "README.md": f"# `{NODE_NAME}-{suffix}`\n".encode(),
            "LICENSE": LICENSE.read_bytes(), binary: b"\x7fELF"}


def self_test() -> int:
    """Synthetic archives with one known defect each, judged by `archive`."""
    version = workspace_version()
    pins = {f"{NODE_NAME}-{suffix}": version for suffix in napi_suffixes()}
    crlf = LICENSE.read_bytes().replace(b"\n", b"\r\n")
    failures: list[str] = []
    ran = 0
    with tempfile.TemporaryDirectory(prefix="package-inspect-selftest-") as tmp:
        base = pathlib.Path(tmp)

        def judge(name: str, paths: list, want: str, *needles: str, release_set: bool = False) -> None:
            nonlocal ran
            ran += 1
            argv = ["archive", *map(str, paths), *(["--release-set"] if release_set else [])]
            host = Host(build_parser().parse_args(argv))
            try:
                with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    cmd_archive(host)
            finally:
                shutil.rmtree(host.scratch, ignore_errors=True)
            got = FAIL if any(a.status == FAIL for a in host.artifacts) else PASS
            failed = " · ".join(f"{c.name}: {c.detail}" for a in host.artifacts for c in a.checks if not c.ok)
            missing = [n for n in needles if n not in failed]
            if got != want or missing:
                failures.append(f"{name}: {got.upper()} (want {want.upper()})"
                                + (f", no failing check names {missing}" if missing else "")
                                + (f" — failing: {failed}" if failed else ""))

        def crate(name: str, files: dict) -> pathlib.Path:
            return _tar(base / f"{name}.crate", f"qrcode-ai-scanner-{version}", files)

        def wheel(name: str, files: dict) -> pathlib.Path:
            return _zip(base / name / f"qrcode_ai_scanner-{version}-cp38-abi3-manylinux_2_17_x86_64.whl", files)

        def sdist(name: str, files: dict) -> pathlib.Path:
            return _tar(base / name / f"qrcode_ai_scanner-{version}.tar.gz", f"qrcode_ai_scanner-{version}", files)

        def npm(name: str, files: dict) -> pathlib.Path:
            pkg = json.loads(files["package.json"])
            return _tar(base / name / f"{pkg['name'].lstrip('@').replace('/', '-')}-{version}.tgz", "package",
                        files)

        for label in ("crate", "wheel", "sdist"):
            (base / label).mkdir()
        (base / "platform").mkdir()
        # The clean archives pass: every negative below fails for its plant.
        judge("clean crate", [crate("clean", _clean_crate(version))], PASS)
        judge("clean wheel", [wheel("wheel", _clean_wheel(version))], PASS)
        judge("clean sdist", [sdist("sdist", _clean_sdist(version))], PASS)
        judge("clean platform package", [npm("platform", _npm_platform(version, "linux-x64-gnu"))], PASS)

        for i, planted in enumerate(PLANTED):
            ran += 1
            if not any(pattern.search(planted) for pattern, _why in STRAY):
                failures.append(f"denylist: {planted!r} matches no STRAY pattern")
            judge(f"crate + {planted}", [crate(f"stray-{i}", _clean_crate(version) | {planted: b"x"})],
                  FAIL, planted)
            (base / f"wheel-{i}").mkdir()
            judge(f"wheel + qrcode_ai_scanner/{planted}",
                  [wheel(f"wheel-{i}", _clean_wheel(version) | {f"qrcode_ai_scanner/{planted}": b"x"})],
                  FAIL, planted)
            (base / f"sdist-{i}").mkdir()
            judge(f"sdist + qrcode-ai-scanner-py/{planted}",
                  [sdist(f"sdist-{i}", _clean_sdist(version) | {f"qrcode-ai-scanner-py/{planted}": b"x"})],
                  FAIL, planted)
            (base / f"platform-{i}").mkdir()
            judge(f"platform package + {planted}",
                  [npm(f"platform-{i}", _npm_platform(version, "linux-x64-gnu") | {planted: b"x"})],
                  FAIL, planted)

        # Untracked but innocuous files: the git allowlist alone must catch them.
        judge("crate + an untracked source file", [crate("untracked", _clean_crate(version) | {"src/scratch.rs": b""})],
              FAIL, "src/scratch.rs")
        (base / "wheel-extra").mkdir()
        judge("wheel + a file outside the wheel allowlist",
              [wheel("wheel-extra", _clean_wheel(version) | {"qrcode_ai_scanner/notes.txt": b""})],
              FAIL, "qrcode_ai_scanner/notes.txt")
        (base / "sdist-extra").mkdir()
        judge("sdist + an untracked core file",
              [sdist("sdist-extra", _clean_sdist(version) | {"qrcode-ai-scanner/src/extra.rs": b""})],
              FAIL, "qrcode-ai-scanner/src/extra.rs")

        # The AGPL text where PEP 639 says it is, byte for byte.
        (base / "wheel-crlf").mkdir()
        judge("win wheel with a CRLF LICENSE",
              [wheel("wheel-crlf", _clean_wheel(version) | {
                  f"qrcode_ai_scanner-{version}.dist-info/licenses/LICENSE": crlf})], FAIL, "LICENSE")
        no_root = {k: v for k, v in _clean_sdist(version).items() if k != "LICENSE"}
        (base / "sdist-py-license-only").mkdir()
        judge("sdist with the AGPL text only under qrcode-ai-scanner-py/",
              [sdist("sdist-py-license-only", no_root)], FAIL, "License-File")
        (base / "sdist-crlf").mkdir()
        judge("sdist whose License-File is a CRLF copy",
              [sdist("sdist-crlf", _clean_sdist(version) | {"LICENSE": crlf})], FAIL, "License-File")
        globbed = _clean_sdist(version)
        globbed["pyproject.toml"] = globbed["pyproject.toml"].replace(b'license-files = ["LICENSE"]',
                                                                      b'license-files = ["LICENSES/*.txt"]')
        (base / "sdist-glob").mkdir()
        judge("sdist whose pyproject license-files matches nothing", [sdist("sdist-glob", globbed)],
              FAIL, "license-files")

        # The main npm package pins exactly every napi target at this version.
        for label, optional, want, release_set, needle in (
            ("main, dev build (no pins)", None, PASS, False, ""),
            ("main, every target pinned", pins, PASS, False, ""),
            ("main, release set without pins", None, FAIL, True, "optionalDependencies"),
            ("main, one target missing", {k: v for k, v in pins.items() if not k.endswith("-linux-x64-musl")},
             FAIL, True, "linux-x64-musl"),
            ("main, one pin at another version", pins | {f"{NODE_NAME}-darwin-arm64": "0.0.1"}, FAIL, True,
             "darwin-arm64"),
            ("main, an unknown extra pin", pins | {f"{NODE_NAME}-linux-riscv64-gnu": version}, FAIL, True,
             "linux-riscv64-gnu"),
            ("main, dev build with a stale pin", pins | {f"{NODE_NAME}-win32-x64-msvc": "0.8.1"}, FAIL, False,
             "win32-x64-msvc"),
        ):
            slug = re.sub(r"[^a-z0-9]+", "-", label)
            (base / slug).mkdir()
            judge(label, [npm(slug, _npm_main(version, optional))], want, *filter(None, [needle]),
                  release_set=release_set)
        # --release-set over tarballs: one platform tarball per pin, no more.
        (base / "set").mkdir()
        main = npm("set", _npm_main(version, pins))
        platforms = []
        for suffix in napi_suffixes():
            (base / f"set-{suffix}").mkdir()
            platforms.append(npm(f"set-{suffix}", _npm_platform(version, suffix)))
        judge("release set: main + one tarball per pin", [main, *platforms], PASS, release_set=True)
        judge("release set: a pinned platform tarball missing", [main, *platforms[:-1]], FAIL,
              napi_suffixes()[-1], release_set=True)

    for failure in failures:
        print(f"SELF-TEST FAILED · {failure}", file=sys.stderr)
    print(f"self-test: {ran - len(failures)}/{ran} scenarios as expected")
    return 1 if failures else 0


# ---------------------------------------------------------------- report


def git_head() -> str:
    proc = subprocess.run(["git", "rev-parse", "--short=12", "HEAD"], cwd=ROOT, capture_output=True, text=True)
    dirty = subprocess.run(["git", "status", "--porcelain"], cwd=ROOT, capture_output=True, text=True)
    return proc.stdout.strip() + (" (dirty)" if dirty.stdout.strip() else "")


def markdown(host: Host, title: str) -> str:
    lines = [f"## package-inspect · {title}", "",
             f"HEAD {git_head()} · workspace {host.version} · "
             f"{datetime.datetime.now(datetime.timezone.utc):%Y-%m-%d %H:%M} UTC", "",
             "| artifact | verdict | archive | sha256 | files |", "|---|---|---|---|---|"]
    for art in host.artifacts:
        lines.append(f"| {art.name} | **{art.status.upper()}** | {art.path or '—'} | "
                     f"{art.sha256[:16] or '—'} | {len(art.listing) or '—'} |")
    for art in host.artifacts:
        lines += ["", f"### {art.name} — {art.status.upper()}", ""]
        if art.reason:
            lines += [f"> {art.status.upper()}: {art.reason}", ""]
        lines += [f"- {note}" for note in art.notes]
        if art.checks:
            lines += ["", "| check | result | detail |", "|---|---|---|"]
            lines += [f"| {c.name} | {'ok' if c.ok else '**FAIL**'} | {c.detail.replace('|', '/')} |"
                      for c in art.checks]
        if art.listing:
            lines += ["", f"<details><summary>{len(art.listing)} entries</summary>", "", "```",
                      *art.listing, "```", "", "</details>"]
    return "\n".join(lines) + "\n"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("target", choices=["all", "crates", "node", "wasm", "python", "flutter",
                                           "archive", "licenses", "profiles", "versions", "selftest"])
    parser.add_argument("files", nargs="*", help="archive: files to inspect")
    parser.add_argument("--out", help="write package-inspect.{json,md} + logs/ here")
    parser.add_argument("--strict", action="store_true", help="UNAVAILABLE fails too")
    parser.add_argument("--quiet", action="store_true", help="stdout: the verdict table and failed checks only")
    parser.add_argument("--cargo-wrapper", help="run cargo as `WRAPPER cargo ...`")
    parser.add_argument("--allow-dirty", action="store_true", help="pass --allow-dirty to cargo package")
    parser.add_argument("--verify", action="store_true", help="crates: build each crate from its archive")
    parser.add_argument("--no-build", action="store_true", help="wasm: inspect the existing pkg/")
    parser.add_argument("--platform-dirs", help="node: a dir of napi platform packages (npm/)")
    parser.add_argument("--flutter", help="flutter: the flutter (or dart) binary")
    parser.add_argument("--sync", action="store_true", help="licenses: rewrite stale copies")
    parser.add_argument("--nightly", help="profiles: the nightly toolchain for --unit-graph")
    parser.add_argument("--tag", help="versions: the release tag (vX.Y.Z or refs/tags/vX.Y.Z)")
    parser.add_argument("--release-set", action="store_true",
                        help="node · archive: the npm set about to publish — the main package pins every napi "
                             "target at this version, and every pin has its platform package")
    return parser


def main() -> int:
    args = build_parser().parse_args()
    if args.target == "selftest":
        return self_test()

    host = Host(args)
    try:
        {
            "all": cmd_all, "crates": cmd_crates, "node": cmd_node, "wasm": cmd_wasm,
            "python": cmd_python, "flutter": cmd_flutter, "archive": cmd_archive,
            "licenses": cmd_licenses, "profiles": cmd_profiles, "versions": cmd_versions,
        }[args.target](host)
    finally:
        shutil.rmtree(host.scratch, ignore_errors=True)

    report = markdown(host, args.target)
    if args.quiet:
        print("\n".join(line for line in report.splitlines()[:6 + len(host.artifacts)] if line.startswith("|")))
        for art in host.artifacts:
            failed = [c for c in art.checks if not c.ok]
            if failed or art.reason:
                print(f"{art.name} — {art.status.upper()}{': ' + art.reason if art.reason else ''}")
                print("\n".join(f"  ✗ {c.name}: {c.detail}" for c in failed))
    else:
        print(report)
    if host.out:
        (host.out / "package-inspect.md").write_text(report)
        (host.out / "package-inspect.json").write_text(json.dumps(
            [dataclasses.asdict(a) | {"status": a.status} for a in host.artifacts], indent=2) + "\n")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as sink:
            sink.write(report)
    failing = {FAIL} | ({UNAVAILABLE} if args.strict else set())
    bad = [a for a in host.artifacts if a.status in failing]
    annotate = "::error title=package-inspect::" if os.environ.get("GITHUB_ACTIONS") else "package-inspect: "
    for art in bad:
        print(f"{annotate}{art.name}: {art.status.upper()} "
              f"{art.reason or '; '.join(c.name for c in art.checks if not c.ok)}", file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
