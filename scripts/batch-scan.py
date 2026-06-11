#!/usr/bin/env python3
"""Batch-scan a directory of images with the qrscan CLI and print a report.

Usage: scripts/batch-scan.py <dir-or-files...> [--bin target/release/qrscan]
                             [--json out.json] [--budget-ms N]

Exit code 0 always (it's a reporter, not a gate) — read the table.
"""

import argparse
import json
import pathlib
import subprocess
import sys

IMG_EXT = {".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp"}


def scan(binary: str, path: pathlib.Path, budget_ms: int | None) -> dict:
    cmd = [binary, str(path)]
    if budget_ms:
        cmd += ["--budget-ms", str(budget_ms)]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    out: dict = {"file": str(path), "exit": proc.returncode}
    try:
        report = json.loads(proc.stdout)
    except json.JSONDecodeError:
        out["error"] = (proc.stderr or proc.stdout)[:200]
        return out
    dets = report.get("detections", [])
    out["detections"] = len(dets)
    if dets:
        d = dets[0]
        out["text"] = d["content"]["text"]
        out["engines"] = d.get("engines", [])
        meta = d.get("meta", {})
        out["version"] = meta.get("version")
        out["ec"] = meta.get("ec_level")
    score = report.get("score")
    if score:
        out["score"] = score["value"]
        out["grade"] = score["grade"]
        uec = score.get("uec")
        if uec is not None:
            out["uec"] = uec
        out["axes"] = {a["axis"]: f"{a['passed']}/{a['total']}" for a in score.get("axes", [])}
    trace = report.get("trace", {})
    out["ms"] = round(trace.get("total_ms", 0.0))
    stages = trace.get("stages", [])
    if stages and dets:
        out["stage"] = stages[-1]["stage"]
    out["hints"] = [h.get("code", "?") for h in report.get("hints", [])]
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("paths", nargs="+")
    ap.add_argument("--bin", default="target/release/qrscan")
    ap.add_argument("--json", dest="json_out")
    ap.add_argument("--budget-ms", type=int, default=None)
    args = ap.parse_args()

    files: list[pathlib.Path] = []
    for p in args.paths:
        path = pathlib.Path(p)
        if path.is_dir():
            files += sorted(f for f in path.rglob("*") if f.suffix.lower() in IMG_EXT)
        elif path.exists():
            files.append(path)
    if not files:
        sys.exit("no images found")

    results = [scan(args.bin, f, args.budget_ms) for f in files]

    decoded = [r for r in results if r.get("detections", 0) > 0]
    failed = [r for r in results if r.get("detections", 0) == 0 and "error" not in r]
    errors = [r for r in results if "error" in r]

    name_w = max(len(pathlib.Path(r["file"]).name) for r in results)
    print(f"{'file':<{name_w}}  {'dec':<3} {'score':<5} {'grade':<9} {'ms':>6}  {'stage':<8} text")
    for r in results:
        name = pathlib.Path(r["file"]).name
        if "error" in r:
            print(f"{name:<{name_w}}  ERR   -     -      {'-':>6}  -        {r['error'][:60]}")
            continue
        dec = "✓" if r.get("detections") else "✗"
        score = str(r.get("score", "-"))
        grade = str(r.get("grade", "-"))
        stage = str(r.get("stage", "-"))
        text = (r.get("text") or "")[:50]
        print(f"{name:<{name_w}}  {dec:<3} {score:<5} {grade:<9} {r['ms']:>6}  {stage:<8} {text}")

    n = len(results)
    avg_ms = sum(r.get("ms", 0) for r in results) / max(n, 1)
    print(f"\ndecoded {len(decoded)}/{n}  ({len(failed)} miss · {len(errors)} err) · avg {avg_ms:.0f}ms")
    if args.json_out:
        pathlib.Path(args.json_out).write_text(json.dumps(results, indent=2))
        print(f"json → {args.json_out}")


if __name__ == "__main__":
    main()
