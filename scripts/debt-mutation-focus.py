#!/usr/bin/env python3
"""Qualify thirteen fixed test debts; this does not replace the full deep gate."""
import argparse
import copy
import hashlib
import json
import pathlib
import re
import subprocess
import tempfile

PREFIX = "crates/qrcode-ai-scanner/src/"
COHORTS = {
    "geometry-scoring": (
        "ladder.rs:478:25: replace > with >= in Rung::apply",
        "score/mod.rs:482:32: delete - in run_lighting",
        "score/mod.rs:533:61: replace - with / in run_axes",
        "score/mod.rs:544:44: replace - with / in run_axes",
        "score/warp.rs:95:40: replace + with * in rotate",
        "score/warp.rs:98:28: replace - with / in rotate",
        "score/warp.rs:98:45: replace - with / in rotate",
        "score/warp.rs:99:39: replace - with / in rotate",
    ),
    "engine-rescue": (
        "engine/rqrr_engine.rs:111:13: delete match arm (EciEvidence::Unobserved, Some(ec)) in decode",
        "engine/rqrr_engine.rs:244:39: replace == with != in rescue_input",
        "rescue/mod.rs:250:43: replace < with <= in attempt",
    ),
    "rgba-allocation": (
        "transform.rs:239:29: replace / with % in flatten",
        "transform.rs:239:29: replace / with * in flatten",
    ),
}
TESTS = {
    "geometry-scoring": "test(/^ladder::/) or test(/^score::/)",
    "engine-rescue": "test(/^engine::rqrr_engine::/) or test(/^rescue::/)",
    "rgba-allocation": "test(/^transform::/)",
}
EXPECTED = {k: tuple(PREFIX + x for x in v) for k, v in COHORTS.items()}
ARTIFACTS = tuple(label + suffix for label in ("version", "nextest-version", "listing", "tests", "command")
                  for suffix in (".stdout", ".stderr")) + ("selected-tests.json", "mutants.out/outcomes.json")


def require(value, message):
    if not value:
        raise ValueError(message)


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def sha(value):
    return hashlib.sha256(value).hexdigest()


def capture(command, out, label):
    result = subprocess.run(command, capture_output=True, check=False)
    (out / (label + ".stdout")).write_bytes(result.stdout)
    (out / (label + ".stderr")).write_bytes(result.stderr)
    return result


def read_json(path):
    return json.loads(path.read_bytes().decode("utf-8"))


def selected_tests(discovered):
    selected = sorted(binary + "::" + name for binary, suite in discovered["rust-suites"].items()
                      for name, test in suite["testcases"].items()
                      if test["filter-match"]["status"] == "matches" and test["ignored"] is False)
    require(selected and len(selected) == len(set(selected)), "empty/duplicate runnable test suite")
    return selected


def mutation_command(cohort, out):
    expected = EXPECTED[cohort]
    files = sorted({name.split(":", 1)[0] for name in expected})
    filters = [value for name in expected for value in ("-F", "^" + re.escape(name) + "$")]
    selectors = [value for file in files for value in ("--file", file)]
    return ["cargo", "mutants", "-p", "qrcode-ai-scanner", "--in-place", "--cargo-arg=--locked",
            "--test-tool", "nextest", "--timeout", "180", "--build-timeout", "900", "-o", str(out)] + \
        selectors + filters + ["--", "--all-features", "--lib", "-E", TESTS[cohort]]


def artifact_hashes(directory):
    return {file: {"sha256": sha((directory / file).read_bytes()), "bytes": (directory / file).stat().st_size}
            for file in ARTIFACTS}


def run(args):
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    require(re.fullmatch(r"[a-f0-9]{40}", args.expected_sha), "expected SHA must be immutable")
    require(revision == args.expected_sha, "checkout revision differs")
    require(not subprocess.check_output(["git", "status", "--porcelain"], text=True).strip(), "dirty input source")
    expected = EXPECTED[args.cohort]
    files = sorted({name.split(":", 1)[0] for name in expected})
    hashes = {}
    for file in files:
        data = pathlib.Path(file).read_bytes()
        # These six files keep their production before the first test MODULE;
        # earlier cfg(test) instrumentation is intentionally retained in the hash.
        boundary = re.search(rb"(?m)^#\[cfg\(test\)\]\n(?:pub\(crate\) )?mod \w+ \{", data)
        require(boundary is not None, "missing test module: " + file)
        hashes[file] = {"source_sha256": sha(data), "production_prefix_sha256": sha(data[:boundary.start()]),
                        "test_module_boundary": boundary.start()}
    filters = [value for name in expected for value in ("-F", "^" + re.escape(name) + "$")]
    selectors = [value for file in files for value in ("--file", file)]
    version = capture(["cargo", "mutants", "--version"], out, "version")
    require(version.returncode == 0 and version.stdout.strip() == b"cargo-mutants 27.1.0", "wrong mutant tool")
    nextest = capture(["cargo", "nextest", "--version"], out, "nextest-version")
    require(nextest.returncode == 0 and re.match(rb"cargo-nextest 0\.9\.116(?:\s|$)", nextest.stdout), "wrong nextest tool")
    listing = capture(["cargo", "mutants", "--list", "--json"] + selectors + filters, out, "listing")
    require(listing.returncode == 0, "mutant listing failed")
    names = [x["name"] for x in json.loads(listing.stdout.decode("utf-8"))]
    require(len(names) == len(expected) and set(names) == set(expected), "changed or incomplete cohort anchors")
    tests = capture(["cargo", "nextest", "list", "-p", "qrcode-ai-scanner", "--locked", "--all-features", "--lib",
                     "--message-format", "json", "-E", TESTS[args.cohort]], out, "tests")
    require(tests.returncode == 0, "test discovery failed")
    discovered = json.loads(tests.stdout.decode("utf-8"))
    selected = selected_tests(discovered)
    save(out / "selected-tests.json", selected)
    command = mutation_command(args.cohort, out)
    result = capture(command, out, "command")
    source_after = {file: sha(pathlib.Path(file).read_bytes()) for file in files}
    clean_after = not subprocess.check_output(["git", "status", "--porcelain"], text=True).strip()
    save(out / "receipt.json", {"schema": "qrscan.debt-cohort.v1", "revision": revision, "cohort": args.cohort,
                               "expected": list(expected), "source_hashes": hashes, "test_expression": TESTS[args.cohort],
                               "command": command, "exit_code": result.returncode, "selected_test_count": len(selected),
                               "source_after_sha256": source_after, "clean_after": clean_after,
                               "harness_sha256": sha(pathlib.Path(__file__).read_bytes()),
                               "harness_source_sha": subprocess.check_output(["git", "rev-parse", "HEAD"],
                                   cwd=pathlib.Path(__file__).resolve().parent.parent, text=True).strip(),
                               "output_directory": str(out), "artifacts": artifact_hashes(out)})
    require(clean_after and all(source_after[file] == hashes[file]["source_sha256"] for file in files),
            "mutation tool did not restore the original clean source")
    require(result.returncode in (0, 2, 3), "baseline/tool failure, not a completed measurement")


def validate(directory, cohort, revision, harness_source):
    receipt = read_json(directory / "receipt.json")
    expected = EXPECTED[cohort]
    require(receipt["schema"] == "qrscan.debt-cohort.v1" and receipt["cohort"] == cohort, "wrong receipt scope")
    require(receipt["revision"] == revision and re.fullmatch(r"[a-f0-9]{40}", revision), "wrong revision")
    require(receipt["expected"] == list(expected) and receipt["selected_test_count"] > 0, "wrong receipt cohort/tests")
    require(receipt["harness_sha256"] == sha(pathlib.Path(__file__).read_bytes())
            and receipt["harness_source_sha"] == harness_source, "wrong harness source")
    require(receipt["artifacts"] == artifact_hashes(directory), "raw artifact digest/size drift")
    require((directory / "version.stdout").read_bytes().strip() == b"cargo-mutants 27.1.0", "wrong mutant version")
    require(re.match(rb"cargo-nextest 0\.9\.116(?:\s|$)", (directory / "nextest-version.stdout").read_bytes()), "wrong nextest version")
    selected = selected_tests(read_json(directory / "tests.stdout"))
    require(read_json(directory / "selected-tests.json") == selected and receipt["selected_test_count"] == len(selected),
            "selected tests/count differ from actual listing")
    require(receipt["test_expression"] == TESTS[cohort] and pathlib.Path(receipt["output_directory"]).is_absolute()
            and receipt["command"] == mutation_command(cohort, receipt["output_directory"]), "command/scope changed")
    files = {name.split(":", 1)[0] for name in expected}
    require(set(receipt["source_hashes"]) == files and set(receipt["source_after_sha256"]) == files,
            "wrong measured source files")
    require(receipt["clean_after"] is True and all(receipt["source_after_sha256"][file]
            == receipt["source_hashes"][file]["source_sha256"] for file in files), "dirty or unrestored source")
    listing = read_json(directory / "listing.stdout")
    names = [x["name"] for x in listing]
    require(len(names) == len(expected) and set(names) == set(expected), "incomplete listing")
    outcomes = read_json(directory / "mutants.out" / "outcomes.json")
    require(outcomes["cargo_mutants_version"] == "27.1.0" and outcomes["end_time"], "wrong tool or incomplete run")
    require(outcomes["total_mutants"] == len(expected) and len(outcomes["outcomes"]) == len(expected) + 1, "incomplete outcomes")
    base = [x for x in outcomes["outcomes"] if x["scenario"] == "Baseline"]
    require(len(base) == 1 and base[0]["summary"] == "Success", "baseline did not pass")
    require([(x["phase"], x["process_status"]) for x in base[0]["phase_results"]]
            == [("Build", "Success"), ("Test", "Success")], "baseline phases incomplete")
    rows = [x for x in outcomes["outcomes"] if isinstance(x["scenario"], dict)]
    names = [x["scenario"]["Mutant"]["name"] for x in rows]
    require(len(names) == len(expected) and set(names) == set(expected), "duplicate/missing/unexpected mutant")
    statuses = {}
    for row in rows:
        name, phases = row["scenario"]["Mutant"]["name"], row["phase_results"]
        status = row["summary"]
        if status in ("CaughtMutant", "MissedMutant"):
            require(len(phases) == 2 and phases[0]["phase"] == "Build" and phases[0]["process_status"] == "Success"
                    and phases[1]["phase"] == "Test", "build failure is not a kill")
            test = phases[1]["process_status"]
            if status == "CaughtMutant":
                require(test == {"Failure": 100} and type(test["Failure"]) is int, "not a nextest test-run failure")
            else:
                require(test == "Success", "miss contradicts test status")
        elif status == "Timeout":
            require((len(phases) == 1 and phases[0]["phase"] == "Build" and phases[0]["process_status"] == "Timeout")
                    or (len(phases) == 2 and phases[0]["phase"] == "Build" and phases[0]["process_status"] == "Success"
                        and phases[1]["phase"] == "Test" and phases[1]["process_status"] == "Timeout"), "contradictory timeout phases")
        elif status == "Unviable":
            require(len(phases) == 1 and phases[0]["phase"] == "Build" and phases[0]["process_status"] == {"Failure": 101},
                    "unviable did not fail the nextest build")
        else:
            raise ValueError("unknown outcome summary: " + status)
        statuses[name] = status
    # The tool reports actual outcome categories; exits alone never establish a kill.
    counters = {"caught": "CaughtMutant", "missed": "MissedMutant", "timeout": "Timeout", "unviable": "Unviable"}
    for key, summary in counters.items():
        require(type(outcomes[key]) is int and outcomes[key] == sum(s == summary for s in statuses.values()), "aggregate counter drift")
    require(outcomes["success"] == 0 and type(outcomes["success"]) is int, "unexpected success counter")
    expected_exit = 3 if outcomes["timeout"] else 2 if outcomes["missed"] else 0
    require(receipt["exit_code"] == expected_exit, "exit contradicts outcome categories")
    return receipt, statuses


def judge(args):
    rows = []
    for cohort in EXPECTED:
        before, old = validate(args.receipts / ("debt-" + cohort + "-before"), cohort, args.baseline_sha, args.candidate_sha)
        after, new = validate(args.receipts / ("debt-" + cohort + "-after"), cohort, args.candidate_sha, args.candidate_sha)
        require(before["source_hashes"].keys() == after["source_hashes"].keys(), "source file set differs")
        for file in before["source_hashes"]:
            require(before["source_hashes"][file]["production_prefix_sha256"]
                    == after["source_hashes"][file]["production_prefix_sha256"], "production prefix changed: " + file)
        for name in EXPECTED[cohort]:
            rows.append({"identity": name, "cohort": cohort, "before": old[name], "after": new[name],
                         "test_kill": new[name] == "CaughtMutant",
                         "new_test_transition": old[name] == "MissedMutant" and new[name] == "CaughtMutant"})
    return {"verdict": "PASS" if all(x["test_kill"] for x in rows) else "INCOMPLETE",
            "scope": "fixed thirteen debts only; successful cohort baselines, exact anchors and production prefixes",
            "baseline_sha": args.baseline_sha, "candidate_sha": args.candidate_sha, "identities": rows,
            "full_deep_qualified": False}


def observe(args):
    rows = []
    for cohort in EXPECTED:
        _, current = validate(args.receipts / ("debt-" + cohort + "-before"), cohort, args.baseline_sha, args.harness_sha)
        rows.extend({"identity": name, "cohort": cohort, "actual_current_outcome": current[name]}
                    for name in EXPECTED[cohort])
    return {"verdict": "BASELINE_MEASURED", "baseline_sha": args.baseline_sha, "identities": rows,
            "scope": "current fixed cohort observations, not new coverage or full deep acceptance"}


def selftest():
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        def counters(value):
            summaries = [x["summary"] for x in value["outcomes"] if isinstance(x["scenario"], dict)]
            value.update({key: summaries.count(summary) for key, summary in {
                "caught": "CaughtMutant", "missed": "MissedMutant", "timeout": "Timeout", "unviable": "Unviable"}.items()})
            value["success"] = 0
        def refresh(directory):
            receipt = read_json(directory / "receipt.json")
            value = read_json(directory / "mutants.out/outcomes.json")
            counters(value)
            save(directory / "mutants.out/outcomes.json", value)
            receipt["exit_code"] = 3 if value["timeout"] else 2 if value["missed"] else 0
            receipt["artifacts"] = artifact_hashes(directory)
            save(directory / "receipt.json", receipt)
        for cohort, expected in EXPECTED.items():
            for revision, source in (("before", "a" * 40), ("after", "b" * 40)):
                out = root / ("debt-" + cohort + "-" + revision)
                (out / "mutants.out").mkdir(parents=True)
                hashes = {n.split(":", 1)[0]: {"production_prefix_sha256": "c" * 64,
                          "source_sha256": "d" * 64} for n in expected}
                save(out / "receipt.json", {"schema": "qrscan.debt-cohort.v1", "revision": source,
                     "cohort": cohort, "expected": list(expected), "source_hashes": hashes,
                     "selected_test_count": 1, "exit_code": 2 if revision == "before" else 0,
                     "source_after_sha256": {file: data["source_sha256"] for file, data in hashes.items()}, "clean_after": True,
                     "harness_sha256": sha(pathlib.Path(__file__).read_bytes()), "harness_source_sha": "b" * 40,
                     "output_directory": str(out), "command": mutation_command(cohort, out), "test_expression": TESTS[cohort]})
                for file in ARTIFACTS:
                    (out / file).write_bytes(b"")
                (out / "version.stdout").write_bytes(b"cargo-mutants 27.1.0\n")
                (out / "nextest-version.stdout").write_bytes(b"cargo-nextest 0.9.116\n")
                save(out / "tests.stdout", {"rust-suites": {"qrscan": {"testcases": {
                    "test_contract": {"filter-match": {"status": "matches"}, "ignored": False},
                    "ignored_probe": {"filter-match": {"status": "matches"}, "ignored": True},
                    "excluded": {"filter-match": {"status": "mismatch"}, "ignored": False}}}}})
                save(out / "selected-tests.json", ["qrscan::test_contract"])
                save(out / "listing.stdout", [{"name": n} for n in expected])
                baseline = {"scenario": "Baseline", "summary": "Success", "phase_results": [
                    {"phase": p, "process_status": "Success"} for p in ("Build", "Test")]}
                outcomes = [baseline]
                for i, name in enumerate(expected):
                    row = {"scenario": {"Mutant": {"name": name}}, "summary": "CaughtMutant",
                           "phase_results": [{"phase": "Build", "process_status": "Success"},
                                             {"phase": "Test", "process_status": {"Failure": 100}}]}
                    if revision == "before" and i % 3 == 0:
                        row["summary"] = "MissedMutant"
                        row["phase_results"][1]["process_status"] = "Success"
                    elif revision == "before" and i % 3 == 1:
                        row["summary"] = "Timeout"
                        row["phase_results"][1]["process_status"] = "Timeout"
                    outcomes.append(row)
                save(out / "mutants.out" / "outcomes.json", {"cargo_mutants_version": "27.1.0",
                     "end_time": "completed", "total_mutants": len(expected), "outcomes": outcomes})
                refresh(out)
        args = argparse.Namespace(receipts=root, baseline_sha="a" * 40, candidate_sha="b" * 40)
        require(judge(args)["verdict"] == "PASS", "mixed historical statuses must be accepted")
        target = root / "debt-rgba-allocation-after" / "mutants.out" / "outcomes.json"
        original = json.loads(target.read_text())
        invalids = []
        value = copy.deepcopy(original); value["outcomes"][0]["summary"] = "Failure"; invalids.append(value)
        value = copy.deepcopy(original); value["outcomes"][1]["phase_results"][0]["process_status"] = {"Failure": 1}; invalids.append(value)
        value = copy.deepcopy(original); value["outcomes"].pop(); invalids.append(value)
        value = copy.deepcopy(original); value["outcomes"][1]["scenario"] = value["outcomes"][2]["scenario"]; invalids.append(value)
        value = copy.deepcopy(original); value["end_time"] = None; invalids.append(value)
        for value in invalids:
            save(target, value)
            refresh(target.parent.parent)
            try:
                judge(args)
            except ValueError:
                pass
            else:
                raise AssertionError("false success accepted")
        for summary in ("MissedMutant", "Timeout", "Unviable"):
            value = copy.deepcopy(original); value["outcomes"][1]["summary"] = summary
            if summary == "MissedMutant":
                value["outcomes"][1]["phase_results"][1]["process_status"] = "Success"
            elif summary == "Timeout":
                value["outcomes"][1]["phase_results"][1]["process_status"] = "Timeout"
            else:
                value["outcomes"][1]["phase_results"] = [{"phase": "Build", "process_status": {"Failure": 101}}]
            save(target, value)
            refresh(target.parent.parent)
            require(judge(args)["verdict"] == "INCOMPLETE", "unproved after result must remain debt")
        for code in (1, 70, 101, 104, 105, 110):
            value = copy.deepcopy(original); value["outcomes"][1]["phase_results"][1]["process_status"] = {"Failure": code}
            save(target, value); refresh(target.parent.parent)
            try:
                judge(args)
            except ValueError:
                pass
            else:
                raise AssertionError("nextest infrastructure error counted as kill")
        save(target, original); refresh(target.parent.parent)
        out = target.parent.parent
        original_tests = (out / "tests.stdout").read_bytes()
        (out / "tests.stdout").write_bytes(b"tamper")
        try:
            judge(args)
        except ValueError:
            pass
        else:
            raise AssertionError("raw byte tamper accepted")
        (out / "tests.stdout").write_bytes(original_tests)
        save(out / "selected-tests.json", [])
        refresh(out)
        try:
            judge(args)
        except ValueError:
            pass
        else:
            raise AssertionError("empty selected tests accepted")
    print("debt-cohort validator: mixed before statuses, incomplete results and sixteen negative controls PASS")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="operation", required=True)
    commands.add_parser("selftest")
    runner = commands.add_parser("run")
    runner.add_argument("--cohort", choices=EXPECTED, required=True)
    runner.add_argument("--expected-sha", required=True)
    runner.add_argument("--out", type=pathlib.Path, required=True)
    verdict = commands.add_parser("judge")
    verdict.add_argument("--baseline-sha", required=True)
    verdict.add_argument("--candidate-sha", required=True)
    verdict.add_argument("--receipts", type=pathlib.Path, required=True)
    observer = commands.add_parser("observe")
    observer.add_argument("--baseline-sha", required=True)
    observer.add_argument("--harness-sha", required=True)
    observer.add_argument("--receipts", type=pathlib.Path, required=True)
    args = parser.parse_args()
    if args.operation == "selftest":
        selftest()
    elif args.operation == "run":
        run(args)
    elif args.operation == "observe":
        print(json.dumps(observe(args), indent=2))
    else:
        result = judge(args)
        print(json.dumps(result, indent=2))
        raise SystemExit(0 if result["verdict"] == "PASS" else 1)


if __name__ == "__main__":
    main()
