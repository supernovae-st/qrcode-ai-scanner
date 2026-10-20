#!/usr/bin/env python3
"""Qualify exact published crates.io packages; no local core substitution."""
import argparse
import base64
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tarfile
import tomllib
import urllib.request
import zlib

VERSION = "0.10.1"
SOURCE = "af440882cd50728a3340eddf089ea2ce89d72a3f"
REGISTRY_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
EXPECTED = {
    "qrcode-ai-scanner": "3968db5db4613403823280f30a3aa79e54457bf7835bdac83dce9fd66ec3712d",
    "qrcode-ai-scanner-cli": "74b5cdc747e4faa5f7d509c49acd0a778270d8734af7d8a305d24ab79b23a517"
}
TEXT = "https://qrcode-ai.com/c/v5q"
CORE_MAIN = r'''use qrcode_ai_scanner::{Charset, ImageInput, ScanError, ScanProfile, ScanReport, Scanner};
use serde_json::Value;
use std::{env, fs, path::Path};

fn validate(schema: &Value, report: &ScanReport) -> Value {
    let value = serde_json::to_value(report).unwrap();
    jsonschema::validator_for(schema)
        .unwrap()
        .validate(&value)
        .unwrap();
    assert_eq!(report.versions.scanner, "0.10.1");
    assert_eq!(report.versions.pipeline, 3);
    assert_eq!(report.versions.score_contract, 5);
    assert_eq!(report.trace.budget_exhausted, Some(false));
    assert_eq!(report.trace.engine_panics, 0);
    value
}

fn scanner(profile: ScanProfile) -> Scanner {
    let mut config = profile.config();
    config.budget_ms = None;
    Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build()
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args[1] == "validate-cli" {
        let schema: Value = serde_json::from_slice(&fs::read(&args[2]).unwrap()).unwrap();
        for file in &args[3..] {
            let report: ScanReport = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
            validate(&schema, &report);
        }
        println!(
            "{{\"verdict\":\"PASS\",\"validated_cli_reports\":{}}}",
            args.len() - 3
        );
        return;
    }
    let root = Path::new(&args[1]);
    let schema: Value =
        serde_json::from_slice(&fs::read(root.join("schema.json")).unwrap()).unwrap();
    let image = fs::read(root.join("fixture.png")).unwrap();
    let full = scanner(ScanProfile::Full);
    let report = full.scan(ImageInput::encoded(&image)).unwrap();
    assert_eq!(report.detections.len(), 1);
    let content = &report.detections[0].content;
    assert_eq!(content.text, "https://qrcode-ai.com/c/v5q");
    assert_eq!(content.raw, b"https://qrcode-ai.com/c/v5q");
    assert_eq!(content.charset, Charset::Utf8);
    assert_eq!(content.eci, None);
    assert_eq!(content.fnc1, None);
    assert_eq!(report.score.as_ref().unwrap().value, 100);
    fs::write(
        root.join("core-full.json"),
        serde_json::to_vec_pretty(&validate(&schema, &report)).unwrap(),
    )
    .unwrap();
    let frame = scanner(ScanProfile::Frame)
        .scan(ImageInput::encoded(&image))
        .unwrap();
    assert_eq!(frame.detections.len(), 1);
    assert_eq!(frame.detections[0].content.raw, content.raw);
    assert_eq!(frame.score, None);
    fs::write(
        root.join("core-frame.json"),
        serde_json::to_vec_pretty(&validate(&schema, &frame)).unwrap(),
    )
    .unwrap();
    let white = [255u8; 64 * 64];
    let empty = full.scan(ImageInput::luma8(&white, 64, 64)).unwrap();
    assert!(empty.detections.is_empty());
    assert_eq!(empty.score, None);
    assert!(empty.hints.is_empty());
    fs::write(
        root.join("core-empty.json"),
        serde_json::to_vec_pretty(&validate(&schema, &empty)).unwrap(),
    )
    .unwrap();
    let invalid = full.scan(ImageInput::encoded(b"not an image")).unwrap_err();
    assert_eq!(invalid.code(), "QRS-001");
    assert!(matches!(invalid, ScanError::InvalidImage { .. }));
    let mismatch = full.scan(ImageInput::rgba8(&[], 64, 64)).unwrap_err();
    assert_eq!(mismatch.code(), "QRS-004");
    assert!(matches!(
        mismatch,
        ScanError::BufferMismatch {
            got: 0,
            expected: 16384
        }
    ));
    println!("{{\"verdict\":\"PASS\",\"core_cases\":5,\"schema_reports\":3}}");
}
'''


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fetch(url):
    request = urllib.request.Request(url, headers={"User-Agent": "qrcode-ai-scanner-registry-consumer/0.10.1"})
    with urllib.request.urlopen(request, timeout=60) as response:
        assert response.status == 200
        return response.read(), response.url


def tracked_hashes(source):
    paths = subprocess.check_output(["git", "ls-files", "-z"], cwd=source).decode().split("\0")
    return {name: sha(source / name) for name in paths if name}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    source, root = args.source.resolve(), args.output.resolve()
    proof = root / "proof"
    proof.mkdir(parents=True)
    commands = []
    receipt = {"verdict": "INCOMPLETE", "version": VERSION, "source_sha": SOURCE,
               "harness_sha": os.environ["GITHUB_SHA"], "workflow_ref": os.environ["GITHUB_WORKFLOW_REF"],
               "target": os.environ["REGISTRY_TARGET"], "started_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
               "cli_registry_install": True, "local_core_substitution": False, "commands": commands}
    (proof / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    env = dict(os.environ, CARGO_HOME=str(root / "cargo-home"), CARGO_TARGET_DIR=str(root / "target"),
               CARGO_TERM_COLOR="never", CARGO_NET_GIT_FETCH_WITH_CLI="false")
    env.pop("CARGO_REGISTRY_TOKEN", None)
    env.pop("RUSTC_BOOTSTRAP", None)
    env.pop("RUSTFLAGS", None)
    env.pop("CARGO_ENCODED_RUSTFLAGS", None)
    assert subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=source, text=True).strip() == SOURCE
    before = tracked_hashes(source)
    (proof / "source-input-sha256.json").write_text(json.dumps(before, indent=2) + "\n")
    target = os.environ["REGISTRY_TARGET"]
    rustc = subprocess.check_output(["rustc", "+1.97.0", "--version", "--verbose"], text=True)
    assert "rustc 1.97.0 " in rustc and "host: " + target in rustc
    (proof / "rustc.txt").write_text(rustc)
    (proof / "cargo.txt").write_text(subprocess.check_output(["cargo", "+1.97.0", "--version"], text=True))

    def command(label, argv, expected=0, cwd=root):
        record = {"label": label, "argv": [str(a) for a in argv], "cwd": str(cwd),
                  "expected_exit_code": expected, "started_utc": datetime.datetime.now(datetime.timezone.utc).isoformat()}
        commands.append(record)
        result = subprocess.run(record["argv"], cwd=cwd, env=env, capture_output=True, text=True, encoding="utf-8")
        (proof / (label + ".stdout")).write_text(result.stdout)
        (proof / (label + ".stderr")).write_text(result.stderr)
        record.update(exit_code=result.returncode, finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat())
        (proof / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
        if result.returncode != expected:
            print(result.stderr[-16000:], flush=True)
            raise RuntimeError(f"{label} exit {result.returncode}, expected {expected}")
        return result

    registry = {}
    for name, expected in EXPECTED.items():
        data, endpoint = fetch(f"https://crates.io/api/v1/crates/{name}/{VERSION}")
        (proof / (name + "-registry.json")).write_bytes(data)
        metadata = json.loads(data)["version"]
        assert metadata["num"] == VERSION and metadata["yanked"] is False
        index_data, index_endpoint = fetch(f"https://index.crates.io/{name[:2]}/{name[2:4]}/{name}")
        (proof / (name + "-index.ndjson")).write_bytes(index_data)
        entries = [json.loads(line) for line in index_data.decode().splitlines()]
        published = [entry for entry in entries if entry["vers"] == VERSION]
        assert len(published) == 1 and published[0]["cksum"] == expected and not published[0]["yanked"]
        archive_data, download = fetch(f"https://static.crates.io/crates/{name}/{name}-{VERSION}.crate")
        archive = proof / (name + "-" + VERSION + ".crate")
        archive.write_bytes(archive_data)
        assert sha(archive) == expected, "registry archive differs from qualified canonical BOM"
        with tarfile.open(archive) as packed:
            members = packed.getmembers()
            assert len({m.name for m in members}) == len(members)
            for member in members:
                path = Path(member.name)
                assert not path.is_absolute() and ".." not in path.parts and "\\" not in member.name
                assert member.isfile() or member.isdir()
                assert path.parts[0] == name + "-" + VERSION
            vcs = json.loads(packed.extractfile(name + "-" + VERSION + "/.cargo_vcs_info.json").read())
            assert vcs["git"]["sha1"] == SOURCE and not vcs["git"].get("dirty", False)
        registry[name] = {"metadata_url": endpoint, "index_url": index_endpoint, "download_url": download,
                          "archive_sha256": expected, "vcs_identity": vcs, "version": VERSION}
    receipt["registry"] = registry
    shutil.copyfile(source / "fixtures/clean/gen_v5_q.png", proof / "fixture.png")
    shutil.copyfile(source / "spec/scan-report.schema.json", proof / "schema.json")
    receipt["fixture_sha256"], receipt["schema_sha256"] = sha(proof / "fixture.png"), sha(proof / "schema.json")
    app = root / "core-application"
    (app / "src").mkdir(parents=True)
    (app / "Cargo.toml").write_text(
        '[package]\nname = "registry-scanner-consumer"\nversion = "0.0.0"\nedition = "2024"\n'
        'rust-version = "1.88"\n[dependencies]\n'
        'qrcode-ai-scanner = { version = "=0.10.1", features = ["serde"] }\n'
        'serde_json = "=1.0.150"\njsonschema = { version = "=0.47.0", default-features = false }\n'
        '[profile.release.package.rxing]\noverflow-checks = true\n')
    (app / "src/main.rs").write_text(CORE_MAIN)
    shutil.copyfile(app / "Cargo.toml", proof / "consumer-Cargo.toml")
    shutil.copyfile(app / "src/main.rs", proof / "consumer-main.rs")
    command("core-generate-lockfile", ["cargo", "+1.97.0", "generate-lockfile"], cwd=app)
    lock_before = sha(app / "Cargo.lock")
    metadata = json.loads(command("core-metadata", ["cargo", "+1.97.0", "metadata", "--locked", "--format-version", "1"], cwd=app).stdout)
    core = [p for p in metadata["packages"] if p["name"] == "qrcode-ai-scanner"]
    assert len(core) == 1 and core[0]["version"] == VERSION and core[0]["source"] == REGISTRY_SOURCE
    assert all(p["source"] == REGISTRY_SOURCE for p in metadata["packages"] if p["id"] != metadata["resolve"]["root"])
    assert not Path(core[0]["manifest_path"]).is_relative_to(source)
    lock = tomllib.loads((app / "Cargo.lock").read_text())
    core_entry = [p for p in lock["package"] if p["name"] == "qrcode-ai-scanner"]
    assert len(core_entry) == 1 and core_entry[0]["checksum"] == EXPECTED["qrcode-ai-scanner"]
    rxing = [p for p in lock["package"] if p["name"] == "rxing"]
    assert len(rxing) == 1 and rxing[0]["version"] == "0.9.1"
    command("core-runtime", ["cargo", "+1.97.0", "run", "--release", "--locked", "--target", target, "--", proof], cwd=app)
    assert json.loads((proof / "core-runtime.stdout").read_text()) == {"verdict": "PASS", "core_cases": 5, "schema_reports": 3}
    assert sha(app / "Cargo.lock") == lock_before
    shutil.copyfile(app / "Cargo.lock", proof / "consumer-Cargo.lock")
    installed = root / "installed"
    command("cli-registry-install", ["cargo", "+1.97.0", "install", "qrcode-ai-scanner-cli", "--version", "=0.10.1", "--locked", "--target", target, "--root", installed])
    binary = installed / "bin" / ("qrscan.exe" if os.name == "nt" else "qrscan")
    assert command("cli-version", [binary, "--version"]).stdout.strip() == "qrscan " + VERSION
    fixture = proof / "fixture.png"
    scan = [binary, fixture, "--profile", "full", "--budget-ms", "0"]
    full = json.loads(command("cli-full", scan).stdout)
    assert full["versions"] == {"scanner": VERSION, "pipeline": 3, "score_contract": 5}
    assert [d["content"] for d in full["detections"]] == [{"text": TEXT, "raw": base64.b64encode(TEXT.encode()).decode(), "charset": "utf8"}]
    assert full["score"]["value"] == 100 and full["trace"]["budget_exhausted"] is False and full["trace"]["engine_panics"] == 0
    assert command("cli-score-only", scan + ["--score-only"]).stdout.strip() == "100"
    frame = json.loads(command("cli-frame", [binary, fixture, "--profile", "frame", "--budget-ms", "0"]).stdout)
    assert frame["score"] is None and [d["content"] for d in frame["detections"]] == [d["content"] for d in full["detections"]]
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    white = proof / "white.png"
    white.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 64, 64, 8, 2, 0, 0, 0))
                      + chunk(b"IDAT", zlib.compress((b"\0" + b"\xff" * 192) * 64)) + chunk(b"IEND", b""))
    empty = json.loads(command("cli-empty", [binary, white, "--profile", "full", "--budget-ms", "0"], expected=1).stdout)
    assert empty["detections"] == [] and empty["score"] is None and empty["hints"] == []
    assert empty["trace"]["budget_exhausted"] is False and empty["trace"]["engine_panics"] == 0
    invalid = proof / "invalid.png"
    invalid.write_bytes(b"not an image")
    result = command("cli-invalid", [binary, invalid], expected=2)
    assert "[QRS-001]" in result.stderr and not result.stdout
    app_binary = root / "target" / target / "release" / ("registry-scanner-consumer.exe" if os.name == "nt" else "registry-scanner-consumer")
    validation = command("cli-schema", [app_binary, "validate-cli", proof / "schema.json", proof / "cli-full.stdout", proof / "cli-frame.stdout", proof / "cli-empty.stdout"])
    assert json.loads(validation.stdout) == {"verdict": "PASS", "validated_cli_reports": 3}
    cli_manifests = list((root / "cargo-home/registry/src").glob("*/qrcode-ai-scanner-cli-0.10.1/Cargo.toml"))
    assert len(cli_manifests) == 1
    cli_manifest = tomllib.loads(cli_manifests[0].read_text())
    assert cli_manifest["dependencies"]["qrcode-ai-scanner"]["version"] == VERSION
    assert cli_manifest["profile"]["release"]["package"]["rxing"]["overflow-checks"] is True
    cli_lock_path = cli_manifests[0].with_name("Cargo.lock")
    cli_lock = tomllib.loads(cli_lock_path.read_text())
    cli_core = [p for p in cli_lock["package"] if p["name"] == "qrcode-ai-scanner"]
    assert len(cli_core) == 1 and cli_core[0]["version"] == VERSION
    assert cli_core[0]["source"] == REGISTRY_SOURCE and cli_core[0]["checksum"] == EXPECTED["qrcode-ai-scanner"]
    shutil.copyfile(cli_lock_path, proof / "installed-cli-Cargo.lock")
    cached_archives = {}
    for name, digest in EXPECTED.items():
        caches = list((root / "cargo-home/registry/cache").glob("*/" + name + "-" + VERSION + ".crate"))
        assert len(caches) == 1 and sha(caches[0]) == digest
        cached_archives[name] = {"path": str(caches[0]), "sha256": digest}
    shutil.copyfile(installed / ".crates2.json", proof / "cargo-install-record.json")
    assert tracked_hashes(source) == before
    receipt.update(verdict="PASS", finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                   all_source_inputs_unchanged=True, core_graph_lock_sha256=lock_before,
                   core_registry_identity=core[0], core_registry_checksum=core_entry[0]["checksum"],
                   core_binary_sha256=sha(app_binary), cli_binary_sha256=sha(binary),
                   cli_executable=str(binary), cli_registry_manifest=str(cli_manifests[0]),
                   cli_locked_core_identity=cli_core[0], cargo_downloaded_archives=cached_archives,
                   cargo_home=env["CARGO_HOME"], cargo_target_dir=env["CARGO_TARGET_DIR"],
                   core_cases=5, cli_reports_schema_validated=3, cli_exit_codes=[0, 1, 2],
                   qualification="Published crates.io-only dependencies and CLI install; no source/path substitution or upload")
    (proof / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt), flush=True)


if __name__ == "__main__":
    try:
        main()
    finally:
        if "--output" in sys.argv:
            proof = Path(sys.argv[sys.argv.index("--output") + 1]).resolve() / "proof"
            if proof.is_dir():
                files = {path.name: {"sha256": sha(path), "bytes": path.stat().st_size}
                         for path in proof.iterdir() if path.is_file() and not path.is_symlink()
                         and path.name != "sha256.json"}
                (proof / "sha256.json").write_text(json.dumps(files, indent=2) + "\n")
