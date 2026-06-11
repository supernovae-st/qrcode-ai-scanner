#!/usr/bin/env python3
"""Run the zxing blackbox QR suites against qrscan and compare to ground truth.

zxing's own harness tests each image at 4 rotations with per-suite pass
thresholds (none are 20/20) — this runner tests 0° only and reports
match/decode/miss per suite, plus the zxing reference threshold for context.

Usage: scripts/zxing-blackbox.py corpus-external/zxing-blackbox [--bin PATH]
"""

import argparse
import json
import pathlib
import subprocess

# zxing reference: mustPassCount at rotation 0° per QRCodeBlackBoxNTestCase.
ZXING_REF = {
    "qrcode-1": 17,
    "qrcode-2": 31,
    "qrcode-3": 38,
    "qrcode-4": 36,
    "qrcode-5": 16,
    "qrcode-6": 15,
}
IMG_EXT = {".png", ".jpg", ".jpeg", ".gif", ".bmp"}


def expected_text(img: pathlib.Path) -> str | None:
    txt = img.with_suffix(".txt")
    if not txt.exists():
        return None
    raw = txt.read_bytes()
    for enc in ("utf-8", "iso-8859-1"):
        try:
            return raw.decode(enc)
        except UnicodeDecodeError:
            continue
    return None


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("root")
    ap.add_argument("--bin", default="target/release/qrscan")
    args = ap.parse_args()
    root = pathlib.Path(args.root)

    grand_match = grand_total = 0
    mismatches: list[str] = []
    for suite in sorted(d for d in root.iterdir() if d.is_dir()):
        images = sorted(f for f in suite.iterdir() if f.suffix.lower() in IMG_EXT)
        match = decoded = 0
        misses: list[str] = []
        for img in images:
            proc = subprocess.run(
                [args.bin, str(img)], capture_output=True, text=True, timeout=120
            )
            try:
                report = json.loads(proc.stdout)
            except json.JSONDecodeError:
                misses.append(f"{img.name} (error)")
                continue
            dets = report.get("detections", [])
            if not dets:
                misses.append(img.name)
                continue
            decoded += 1
            want = expected_text(img)
            texts = [d["content"]["text"] for d in dets]
            if want is not None and want in texts:
                match += 1
            elif want is not None:
                mismatches.append(
                    f"{suite.name}/{img.name}: want {want[:40]!r} got {texts[0][:40]!r}"
                )
        ref = ZXING_REF.get(suite.name, "?")
        flag = "✓" if isinstance(ref, int) and match >= ref else "⚠"
        print(
            f"{suite.name}: match {match}/{len(images)} · decoded {decoded} · "
            f"zxing-ref {ref} {flag} · miss: {', '.join(misses[:6])}"
        )
        grand_match += match
        grand_total += len(images)

    print(f"\nTOTAL ground-truth match: {grand_match}/{grand_total}")
    if mismatches:
        print("\ntext mismatches (decoded but wrong):")
        for m in mismatches:
            print(" ", m)


if __name__ == "__main__":
    main()
