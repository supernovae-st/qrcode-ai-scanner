#!/usr/bin/env python3
"""Prepare a reviewable version patch with Cargo and real CLI goldens.

No commit, tag or publication. --check only validates the example inventory.
"""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[1]
LOCK_ROOTS = (".", "crates/qrcode-ai-scanner-py", "crates/qrcode-ai-scanner-uniffi",
              "bindings/flutter/rust", "fuzz")


def run(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True)


def examples():
    rows = []
    for line in (ROOT / "spec/README.md").read_text().splitlines():
        match = re.match(r"\| \[`(examples/[^`]+\.json)`\].*? \| `([^`]+)` \|", line)
        if match:
            golden, fixture = match.groups()
            fixture_path = (ROOT / fixture).resolve()
            if (".." in Path(golden).parts or Path(fixture).is_absolute()
                    or not fixture_path.is_relative_to(ROOT)):
                raise ValueError("unsafe example path")
            if not fixture_path.is_file():
                raise ValueError(f"missing fixture: {fixture}")
            rows.append(("spec/" + golden, fixture))
    actual = {str(p.relative_to(ROOT)) for p in (ROOT / "spec/examples").glob("*.json")}
    if not rows or len(rows) != len(actual) or {r[0] for r in rows} != actual:
        raise ValueError("example table must name each golden exactly once")
    return rows


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def normalized_lock(text, old, new):
    value = tomllib.loads(text)
    for package in value["package"]:
        if "source" not in package and package["version"] == new:
            package["version"] = old
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--version")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    rows = examples()
    old = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    if args.check:
        print(json.dumps({"version": old, "examples": len(rows), "lockfiles": len(LOCK_ROOTS)}))
        return
    if not args.version or not re.fullmatch(r"\d+\.\d+\.\d+", args.version):
        parser.error("--version must be an exact x.y.z release")
    version = args.version
    if tuple(map(int, version.split("."))) <= tuple(map(int, old.split("."))):
        parser.error("release version must exceed the checkout version")
    if args.output is None or not args.output.is_absolute():
        parser.error("--output must be an absolute directory outside the checkout")
    output = args.output.resolve()
    if output == ROOT or ROOT in output.parents:
        parser.error("output must be outside the checkout")
    if run("git", "status", "--porcelain").strip():
        raise SystemExit("release preparation requires a clean checkout")
    output.mkdir(parents=True, exist_ok=False)
    source_sha = run("git", "rev-parse", "HEAD").strip()
    source_tree = run("git", "rev-parse", "HEAD^{tree}").strip()
    compiler = run("rustc", "--version", "--verbose")
    run("cargo", "build", "--release", "--locked", "-p", "xtask")
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    if not target.is_absolute():
        target = ROOT / target
    xtask = target / "release/xtask"
    run(str(xtask), "sync-version", "--check")
    before_locks = {str(Path(p) / "Cargo.lock"): (ROOT / p / "Cargo.lock").read_text()
                    for p in LOCK_ROOTS}
    manifest = ROOT / "Cargo.toml"
    text, count = re.subn(r'(\[workspace\.package\]\nversion = ")[^"]+("\n)',
                          lambda m: m[1] + version + m[2], manifest.read_text())
    if count != 1:
        raise SystemExit("workspace version source was not unique")
    manifest.write_text(text)
    run(str(xtask), "sync-version")
    for folder in LOCK_ROOTS:
        run("cargo", "metadata", "--format-version", "1", "--manifest-path",
            str(ROOT / folder / "Cargo.toml"))
    for path, original in before_locks.items():
        updated = (ROOT / path).read_text()
        if normalized_lock(original, old, version) != normalized_lock(updated, old, version):
            raise SystemExit(f"dependency resolution changed outside the version bump: {path}")
    changelog = ROOT / "CHANGELOG.md"
    notes = changelog.read_text()
    if notes.count("## Unreleased\n") != 1:
        raise SystemExit("exactly one Unreleased changelog section is required")
    date = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
    changelog.write_text(notes.replace("## Unreleased\n", f"## {version} — {date}\n", 1))
    run("cargo", "build", "--release", "--locked", "-p", "qrcode-ai-scanner-cli")
    cli = target / "release/qrscan"
    for golden, fixture in rows:
        command = [str(cli), fixture, "--profile", "full", "--budget-ms", "0"]
        if golden.endswith("transparent-palette.json"):
            command += ["--alpha-palette", "#f5f5f4,#1c1917"]
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=60)
        if result.returncode not in (0, 1):
            raise SystemExit(f"golden generation failed: {golden}: {result.stderr}")
        report = json.loads(result.stdout)
        if report["versions"]["scanner"] != version or report["trace"]["budget_exhausted"]:
            raise SystemExit(f"incomplete or wrong-version golden: {golden}")
        if bool(report["detections"]) != (result.returncode == 0):
            raise SystemExit(f"CLI exit/report disagreement: {golden}")
        (ROOT / golden).write_text(result.stdout)
    run(str(xtask), "sync-version", "--check")
    run("python3", "scripts/package-inspect.py", "versions", "--tag", "refs/tags/v" + version,
        "--strict", "--quiet")
    run("cargo", "test", "--locked", "-p", "qrcode-ai-scanner", "--test", "spec_golden")
    changed = run("git", "diff", "--name-only").splitlines()
    patch = output / "release.patch"
    patch.write_text(run("git", "diff", "--binary"))
    receipt = {"schema": "qrscan.release-preparation.v1", "source_sha": source_sha,
               "source_tree": source_tree, "version": version, "rustc": compiler,
               "cli_sha256": digest(cli), "xtask_sha256": digest(xtask),
               "patch_sha256": digest(patch), "generated": {p: digest(ROOT / p) for p in changed},
               "examples": len(rows), "locks": list(before_locks),
               "publication": False, "candidate_commit": None}
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    (output / "qrscan").write_bytes(cli.read_bytes())
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    main()
