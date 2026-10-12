#!/usr/bin/env python3
"""Run and judge a fixed mutation cohort without replacing the full deep gate.

The warp cohort is deliberately pinned to eight identities. A moved anchor,
missing baseline, timeout or compile failure cannot become a successful kill.
Run each revision in its own checkout/target, then judge both raw receipts.
"""

import argparse
import copy
import hashlib
import json
import pathlib
import re
import subprocess
import sys
import tempfile

FILE = "crates/qrcode-ai-scanner/src/score/warp.rs"
EXPECTED = tuple(f"{FILE}:{suffix}" for suffix in (
    "24:40: replace - with + in warp_into",
    "24:40: replace - with / in warp_into",
    "24:67: replace - with + in warp_into",
    "24:67: replace - with / in warp_into",
    "33:35: replace match guard sx >= -0.5 && sx <= sw + 0.5 && sy >= -0.5 && sy <= sh + 0.5 with true in warp_into",
    "33:46: replace && with || in warp_into",
    "33:64: replace && with || in warp_into",
    "33:78: replace && with || in warp_into",
))


def require(condition, message):
    if not condition:
        raise ValueError(message)


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def capture(command, stdout, stderr):
    result = subprocess.run(command, text=True, capture_output=True, check=False)
    stdout.write_text(result.stdout)
    stderr.write_text(result.stderr)
    return result


def run(args):
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    cargo = [args.cargo_wrapper, "cargo"] if args.cargo_wrapper else ["cargo"]
    version = capture(cargo + ["mutants", "--version"], out / "version.txt", out / "version.stderr")
    require(version.returncode == 0 and version.stdout.strip() == "cargo-mutants 27.1.0", "requires cargo-mutants 27.1.0")
    filters = [part for name in EXPECTED for part in ("-F", re.escape(name) + "$")]
    listing = capture(cargo + ["mutants", "--list", "--json", "--file", FILE] + filters,
                      out / "listed.json", out / "listing.stderr")
    require(listing.returncode == 0, "mutant listing failed")
    listed = json.loads(listing.stdout)
    names = [item["name"] for item in listed]
    require(len(names) == len(EXPECTED) and set(names) == set(EXPECTED), "cohort anchors changed")
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    source = pathlib.Path(FILE).read_bytes()
    production, separator, _ = source.partition(b"#[cfg(test)]")
    require(bool(separator), "missing test-module boundary")
    command = cargo + ["mutants", "-p", "qrcode-ai-scanner", "--in-place", "--cargo-arg=--locked",
                       "--test-tool", "nextest", "--timeout", "120", "--build-timeout", "900",
                       "--file", FILE, "-o", str(out / "mutants.out")] + filters + [
                           "--", "--all-features", "--lib", "-E", "test(/^score::warp::tests::/)"
                       ]
    result = capture(command, out / "command.stdout", out / "command.stderr")
    write_json(out / "receipt.json", {
        "revision": head, "production_sha256": hashlib.sha256(production).hexdigest(),
        "source_sha256": hashlib.sha256(source).hexdigest(), "command": command,
        "exit_code": result.returncode, "cohort": list(EXPECTED),
    })
    require(result.returncode in (0, 2, 3), f"mutation tool failed: exit {result.returncode}")


def validate(directory, expected_summary, expected_exit):
    receipt = json.loads((directory / "receipt.json").read_text())
    listing = json.loads((directory / "listed.json").read_text())
    result = json.loads((directory / "mutants.out" / "outcomes.json").read_text())
    require(re.fullmatch(r"[0-9a-f]{40}", receipt["revision"]), "invalid source revision")
    require(receipt["exit_code"] == expected_exit, "unexpected mutation exit code")
    require(receipt["cohort"] == list(EXPECTED), "receipt cohort differs")
    names = [item["name"] for item in listing]
    require(len(names) == 8 and set(names) == set(EXPECTED), "incomplete or duplicate listing")
    require(result["cargo_mutants_version"] == "27.1.0" and result["end_time"], "wrong tool or incomplete run")
    baselines = [item for item in result["outcomes"] if item["scenario"] == "Baseline"]
    require(len(baselines) == 1 and baselines[0]["summary"] == "Success", "baseline failed or missing")
    require([(phase["phase"], phase["process_status"]) for phase in baselines[0]["phase_results"]]
            == [("Build", "Success"), ("Test", "Success")], "baseline phases incomplete")
    mutants = [item for item in result["outcomes"] if isinstance(item["scenario"], dict)]
    names = [item["scenario"]["Mutant"]["name"] for item in mutants]
    require(len(names) == 8 and set(names) == set(EXPECTED) and result["total_mutants"] == 8,
            "incomplete or duplicate executed cohort")
    for item in mutants:
        require(item["summary"] == expected_summary, f"unexpected outcome: {item['scenario']['Mutant']['name']}")
        phases = item["phase_results"]
        require(len(phases) == 2 and phases[0]["phase"] == "Build" and phases[0]["process_status"] == "Success",
                "compile failure is not a test kill")
        require(phases[1]["phase"] == "Test", "missing test phase")
        status = phases[1]["process_status"]
        require(status == "Success" if expected_summary == "MissedMutant"
                else isinstance(status, dict) and isinstance(status.get("Failure"), int) and status["Failure"] != 0,
                "test status contradicts verdict")
    return receipt


def judge(before, after):
    old = validate(before, "MissedMutant", 2)
    new = validate(after, "CaughtMutant", 0)
    require(old["revision"] != new["revision"], "before and after use the same revision")
    require(old["production_sha256"] == new["production_sha256"], "production changed: not a tests-only comparison")
    require(old["source_sha256"] != new["source_sha256"], "test source unchanged")
    return {"verdict": "pass", "scope": "warp boundaries only", "before": old["revision"],
            "after": new["revision"], "missed_before": 8, "caught_after": 8, "identities": list(EXPECTED)}


def selftest():
    def fixture(directory, caught):
        directory.mkdir()
        (directory / "mutants.out").mkdir()
        write_json(directory / "receipt.json", {"revision": ("b" if caught else "a") * 40,
                   "production_sha256": "same", "source_sha256": "after" if caught else "before",
                   "exit_code": 0 if caught else 2, "cohort": list(EXPECTED)})
        write_json(directory / "listed.json", [{"name": name} for name in EXPECTED])
        success = [{"phase": phase, "process_status": "Success"} for phase in ("Build", "Test")]
        outcomes = [{"scenario": "Baseline", "summary": "Success", "phase_results": success}]
        for name in EXPECTED:
            phases = copy.deepcopy(success)
            if caught:
                phases[1]["process_status"] = {"Failure": 100}
            outcomes.append({"scenario": {"Mutant": {"name": name}},
                             "summary": "CaughtMutant" if caught else "MissedMutant", "phase_results": phases})
        result = {"cargo_mutants_version": "27.1.0", "end_time": "complete", "total_mutants": 8, "outcomes": outcomes}
        write_json(directory / "mutants.out" / "outcomes.json", result)
        return result

    with tempfile.TemporaryDirectory() as temp:
        root = pathlib.Path(temp)
        before, after = root / "before", root / "after"
        fixture(before, False)
        valid = fixture(after, True)
        require(judge(before, after)["caught_after"] == 8, "valid pair rejected")
        defects = []
        missing = copy.deepcopy(valid); missing["outcomes"].pop(); defects.append(missing)
        duplicate = copy.deepcopy(valid); duplicate["outcomes"][-1] = duplicate["outcomes"][1]; defects.append(duplicate)
        baseline = copy.deepcopy(valid); baseline["outcomes"][0]["summary"] = "Timeout"; defects.append(baseline)
        build = copy.deepcopy(valid); build["outcomes"][1]["phase_results"][0]["process_status"] = {"Failure": 1}; defects.append(build)
        timeout = copy.deepcopy(valid); timeout["outcomes"][1]["summary"] = "Timeout"; defects.append(timeout)
        incomplete = copy.deepcopy(valid); incomplete["end_time"] = None; defects.append(incomplete)
        for defect in defects:
            write_json(after / "mutants.out" / "outcomes.json", defect)
            try:
                judge(before, after)
            except ValueError:
                continue
            raise ValueError("invalid receipt accepted")
    print("PASS: valid comparison and 6 rejected forensic defects")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_subparsers(dest="mode", required=True)
    execute = modes.add_parser("run")
    execute.add_argument("--out", type=pathlib.Path, required=True)
    execute.add_argument("--cargo-wrapper")
    compare = modes.add_parser("judge")
    compare.add_argument("--before", type=pathlib.Path, required=True)
    compare.add_argument("--after", type=pathlib.Path, required=True)
    modes.add_parser("selftest")
    args = parser.parse_args()
    try:
        if args.mode == "run": run(args)
        elif args.mode == "judge": print(json.dumps(judge(args.before, args.after), indent=2))
        else: selftest()
    except (ValueError, KeyError, TypeError, OSError, json.JSONDecodeError, subprocess.SubprocessError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
