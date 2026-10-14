#!/usr/bin/env python3
"""Verify the actual CLI image, including isolated mounted input and exits."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import tempfile
import zlib


def white_png():
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    row = b"\x00" + b"\xff\xff\xff" * 64
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 64, 64, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(row * 64)) + chunk(b"IEND", b""))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image")
    parser.add_argument("fixture", type=Path)
    parser.add_argument("--receipt", type=Path)
    parser.add_argument("--expected-platform", choices=("linux/amd64", "linux/arm64"))
    args = parser.parse_args()
    image = json.loads(subprocess.check_output(["docker", "image", "inspect", args.image], text=True))[0]
    assert image["Config"]["User"] == "65532:65532"
    assert image["Config"]["Entrypoint"] == ["/usr/local/bin/qrscan"]
    assert image["Config"]["Labels"]["org.opencontainers.image.version"] == "0.10.0"
    assert image["Config"]["Labels"]["org.opencontainers.image.licenses"] == "AGPL-3.0-or-later"
    assert image["Architecture"] in ("amd64", "arm64")
    actual_platform = image["Os"] + "/" + image["Architecture"]
    if args.expected_platform:
        assert actual_platform == args.expected_platform, (actual_platform, args.expected_platform)
    fixture = args.fixture.resolve()
    assert hashlib.sha256(fixture.read_bytes()).hexdigest() == "1dc399cad19623a145b20251cf1c0da42ea4f54fd79871a4dd796c9554822dd6"
    cases = []
    with tempfile.TemporaryDirectory(prefix="qrscan-oci-") as directory:
        root = Path(directory)
        root.chmod(0o755)
        shutil.copyfile(fixture, root / "input with spaces.png")
        (root / "white.png").write_bytes(white_png())
        (root / "invalid.bin").write_bytes(b"this is not an image")
        for file in root.iterdir():
            file.chmod(0o644)
        prefix = ["docker", "run", "--rm", "--network", "none", "--read-only", "--cap-drop", "ALL",
                  "--security-opt", "no-new-privileges", "--memory", "512m", "--cpus", "2",
                  "--pids-limit", "128", "--mount", f"type=bind,src={root},dst=/data,readonly", args.image]

        def run(arguments, code):
            result = subprocess.run(prefix + arguments, capture_output=True, text=True, timeout=90)
            cases.append({"arguments": arguments, "exit": result.returncode,
                          "stdout_sha256": hashlib.sha256(result.stdout.encode()).hexdigest(),
                          "stderr": result.stderr})
            assert result.returncode == code, (arguments, result.returncode, result.stderr)
            return result.stdout

        assert run(["--version"], 0).strip() == "qrscan 0.10.0"
        assert "--profile" in run(["--help"], 0)
        full = json.loads(run(["/data/input with spaces.png", "--profile", "full", "--budget-ms", "0"], 0))
        assert full["versions"] == {"scanner": "0.10.0", "pipeline": 3, "score_contract": 5}
        text = "https://qrcode-ai.com/c/v5q"
        assert [d["content"] for d in full["detections"]] == [
            {"text": text, "raw": base64.b64encode(text.encode()).decode(), "charset": "utf8"}]
        assert full["score"]["value"] == 100
        assert full["trace"]["budget_exhausted"] is False
        assert "alpha" not in full
        frame = json.loads(run(["/data/input with spaces.png", "--profile", "frame", "--budget-ms", "0"], 0))
        assert frame["score"] is None and frame["detections"][0]["content"] == full["detections"][0]["content"]
        empty = json.loads(run(["/data/white.png", "--profile", "full", "--budget-ms", "0"], 1))
        assert empty["detections"] == [] and empty["score"] is None
        assert run(["/data/input with spaces.png", "--budget-ms", "0", "--score-only"], 0).strip() == "100"
        assert run(["/data/input with spaces.png", "--profile", "frame", "--score-only"], 2) == ""
        assert run(["/data/invalid.bin"], 2) == ""
        assert run(["/data/absent.png"], 2) == ""
        assert run(["/data/input with spaces.png", "--profile", "invalid"], 2) == ""
        permission = subprocess.run(prefix[:-1] + ["--entrypoint", "/bin/sh", args.image,
            "-c", "test $(id -u) -eq 65532 && test ! -w /data && test ! -w /usr/local/bin"],
            capture_output=True, text=True, timeout=30)
        assert permission.returncode == 0, permission.stderr
    receipt = {"image": args.image, "image_id": image["Id"], "platform": actual_platform,
               "expected_platform": args.expected_platform,
               "host": platform.platform(), "source": image["Config"]["Labels"]["org.opencontainers.image.revision"],
               "packaging_revision": os.environ.get("GITHUB_SHA"), "cases": cases,
               "runtime_permissions": "nonroot/read-only/no-network/no-capabilities", "result": "PASS"}
    if args.receipt:
        args.receipt.parent.mkdir(parents=True, exist_ok=True)
        args.receipt.write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    main()
