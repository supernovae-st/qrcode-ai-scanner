#!/usr/bin/env python3
"""Exercise inspected Cargo archives outside the checkout before publication.

The CLI's unavailable registry core is substituted with its exact archive.
This is archive qualification, not a post-publish cargo install --locked proof.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import struct
import tarfile
import tempfile
import tomllib
import zlib


def run(args, cwd, env):
    print(json.dumps(args), flush=True)
    return subprocess.check_output(args, cwd=cwd, env=env, text=True)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archives", type=Path)
    parser.add_argument("fixture", type=Path)
    parser.add_argument("version")
    args = parser.parse_args()
    consumer = Path(tempfile.mkdtemp(prefix="qrscan-packed-rust-"))
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(consumer / "target")
    env["RAYON_NUM_THREADS"] = "1"
    archives = {}
    for name in ("qrcode-ai-scanner", "qrcode-ai-scanner-cli"):
        file = (args.archives / f"{name}-{args.version}.crate").resolve()
        archives[name] = {"path": str(file), "sha256": sha(file)}
        with tarfile.open(file) as archive:
            archive.extractall(consumer, filter="data")
    core = consumer / f"qrcode-ai-scanner-{args.version}"
    cli = consumer / f"qrcode-ai-scanner-cli-{args.version}"
    for root in (core, cli):
        package = tomllib.loads((root / "Cargo.toml").read_text())["package"]
        assert package["version"] == args.version
    fixture = consumer / "fixture.png"
    shutil.copyfile(args.fixture.resolve(), fixture)
    app = consumer / "application"
    (app / "src").mkdir(parents=True)
    (app / "Cargo.toml").write_text(
        '[package]\nname = "packed-scanner-consumer"\nversion = "0.0.0"\nedition = "2024"\n'
        'rust-version = "1.88"\n[dependencies]\nqrcode-ai-scanner = { path = '
        + json.dumps(str(core)) + ', features = ["serde"] }\n'
        '[profile.release.package.rxing]\noverflow-checks = true\n')
    (app / "src/main.rs").write_text('''use qrcode_ai_scanner::{ImageInput, ScanProfile, Scanner};
fn main() {
    let data = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let mut config = ScanProfile::Full.config();
    config.budget_ms = None;
    let scanner = Scanner::builder().profile(ScanProfile::Custom(config)).build();
    let report = scanner.scan(ImageInput::encoded(&data)).unwrap();
    assert_eq!(report.detections.len(), 1);
    assert_eq!(report.detections[0].content.text, "https://qrcode-ai.com/c/v5q");
    assert_eq!(report.score.as_ref().unwrap().value, 100);
    assert_eq!(report.trace.budget_exhausted, Some(false));
}
''')
    shutil.copyfile(core / "Cargo.lock", app / "Cargo.lock")
    original_core_lock = sha(core / "Cargo.lock")
    registry_before = {
        (p["name"], p["version"], p["source"]): p.get("checksum")
        for p in tomllib.loads((core / "Cargo.lock").read_text())["package"] if "source" in p
    }
    graphs = {}
    # Cargo adds this consumer root to the packaged graph; no registry package
    # is replaced by checkout source. The second graph intentionally re-resolves.
    run(["cargo", "metadata", "--format-version", "1"], app, env)
    registry_after = {
        (p["name"], p["version"], p["source"]): p.get("checksum")
        for p in tomllib.loads((app / "Cargo.lock").read_text())["package"] if "source" in p
    }
    # Cargo may prune the package's development-only dependencies. Every
    # retained registry dependency must be the same pinned, checksummed one.
    assert all(key in registry_before and checksum == registry_before[key]
               for key, checksum in registry_after.items()), "packaged dependency graph drifted"
    graphs["packaged"] = sha(app / "Cargo.lock")
    run(["cargo", "run", "--release", "--locked", "--", str(fixture)], app, env)
    (app / "Cargo.lock").unlink()
    run(["cargo", "generate-lockfile"], app, env)
    graphs["latest-compatible"] = sha(app / "Cargo.lock")
    run(["cargo", "run", "--release", "--locked", "--", str(fixture)], app, env)
    # The normalized CLI pins a core not yet live. Modify only this extracted
    # scratch copy; retain both original archive/lock hashes in the receipt.
    cli_lock = sha(cli / "Cargo.lock")
    patch = 'patch.crates-io.qrcode-ai-scanner.path=' + json.dumps(str(core))
    run(["cargo", "metadata", "--format-version", "1", "--config", patch], cli, env)
    run(["cargo", "install", "--path", str(cli), "--locked", "--root", str(consumer / "installed"),
         "--config", patch], consumer, env)
    binary = consumer / "installed/bin" / ("qrscan.exe" if os.name == "nt" else "qrscan")
    version = run([str(binary), "--version"], consumer, env).strip()
    assert version == f"qrscan {args.version}", version
    command = [str(binary), str(fixture), "--profile", "full", "--budget-ms", "0"]
    report = json.loads(run(command, consumer, env))
    text = "https://qrcode-ai.com/c/v5q"
    assert report["versions"] == {"scanner": args.version, "pipeline": 3, "score_contract": 5}
    assert [d["content"] for d in report["detections"]] == [
        {"text": text, "raw": base64.b64encode(text.encode()).decode(), "charset": "utf8"}]
    assert report["score"]["value"] == 100 and report["trace"]["budget_exhausted"] is False
    assert run(command + ["--score-only"], consumer, env).strip() == "100"
    frame = json.loads(run([str(binary), str(fixture), "--profile", "frame", "--budget-ms", "0"], consumer, env))
    assert frame["score"] is None and [d["content"]["text"] for d in frame["detections"]] == [text]
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    white = consumer / "white.png"
    white.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 64, 64, 8, 2, 0, 0, 0))
                      + chunk(b"IDAT", zlib.compress((b"\0" + b"\xff" * 192) * 64)) + chunk(b"IEND", b""))
    empty = subprocess.run([str(binary), str(white), "--profile", "full", "--budget-ms", "0"],
                           cwd=consumer, env=env, capture_output=True, text=True)
    empty_report = json.loads(empty.stdout)
    assert empty.returncode == 1 and empty_report["detections"] == [] and empty_report["score"] is None
    assert empty_report["trace"]["budget_exhausted"] is False
    invalid = consumer / "invalid.png"
    invalid.write_bytes(b"not an image")
    result = subprocess.run([str(binary), str(invalid)], cwd=consumer, env=env, capture_output=True, text=True)
    assert result.returncode == 2 and "[QRS-001]" in result.stderr and not result.stdout
    receipt = {"verdict": "PASS", "version": args.version, "consumer": str(consumer),
               "archives": archives, "fixture_sha256": sha(fixture), "core_graphs": graphs,
               "core_original_lock_sha256": original_core_lock,
               "core_registry_packages_preserved": len(registry_after),
               "core_profile": "release, caller explicitly enables rxing overflow checks",
               "cli_original_lock_sha256": cli_lock, "cli_substituted_lock_sha256": sha(cli / "Cargo.lock"),
               "cli_sha256": sha(binary), "cli_registry_install": False,
               "qualification": "exact archives with local core substitution; registry replay required"}
    receipt_path = consumer / "receipt.json"
    receipt_path.write_text(json.dumps(receipt, indent=2) + "\n")
    if "GITHUB_OUTPUT" in env:
        with open(env["GITHUB_OUTPUT"], "a") as output:
            output.write(f"receipt={receipt_path}\n")
    print(json.dumps(receipt))


if __name__ == "__main__":
    main()
