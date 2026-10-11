#!/usr/bin/env python3
"""Mutation verdict over sharded cargo-mutants runs — the honest ratchet.

deep-checks.yml shards cargo-mutants 85 ways. The shards used to run under
`continue-on-error`, so weekly run 37262272113 (2026-10-05) concluded success
over 77 missed and 5 timed-out mutants, with 12 of 16 shards exiting 2 or 3 —
and a baseline failure (exit 4) would have read green too. This gate reads
every shard's outcomes.json and judges them together. It FAILS when:

  * a shard directory or its outcomes.json is missing or unreadable (the
    shard died, timed out before its upload, or never started testing) —
    unless cargo-mutants' own mutants.json, the list of the mutants that
    shard was given, is empty and its recorded exit code is 0: that shard
    had nothing to test (fewer mutants than shards), and the report names
    it empty, never a problem;
  * a shard did not finish (no `end_time`: killed mid-run, partial verdict);
  * a baseline did not pass (the unmutated suite is already red, so no
    mutant verdict means anything — cargo-mutants exit 4);
  * a shard recorded an exit code other than 0 · 2 · 3 in
    mutants-exit-code.txt (usage, baseline, filter-diff or internal error);
  * a mutant was tested by two shards (overlapping or duplicated inputs);
  * a MISSED or TIMEOUT mutant has no disposition, or its disposition's
    category does not cover that outcome.

Caught and unviable mutants never need a disposition.

Dispositions (.cargo/mutants-dispositions.toml) are exact anchors: file,
line, column and mutation text — the identity cargo-mutants prints as
`file:line:col: mutation`. Anchored ON PURPOSE, like the exclusions in
.cargo/mutants.toml: a function-scoped match would accept a killable
sibling. An anchor that drifts goes stale in the SAFE direction — the mutant
resurfaces undispositioned, the report names the stale entry it probably
was, and a human re-anchors it. Stale entries are listed, never failed.

An `equivalent` entry can also be anchored by an exclusion in the
cargo-mutants policy (`--exclusions .cargo/mutants.toml`): the run never
generates it, and the entry records why the exclusion exists. With the
policy given, such an entry is listed as excluded and documented, not
stale — when an `exclude_re` pattern matches its name, no shard tested it,
no undispositioned mutant came back with its file and mutation elsewhere,
and the checkout still holds it (the anchored line, and the operator a
`replace OP with …` or `delete OP` mutation names at its column). Stale
stays for an entry the run reports caught or unviable, and for one whose
code moved or is gone; an excluded entry of another category stays stale
too: only an equivalent may be documented by an exclusion.

  category            covers            meaning
  equivalent          MISSED            no deterministic test can tell it apart
  timeout-structural  TIMEOUT           it can only show up as a hang; the
                                        timeout IS the catch
  debt                MISSED, TIMEOUT   a test should kill it — recorded, not
                                        excused

Usage:
  check-mutants-outcome.py --dispositions FILE [--exclusions FILE] [--markdown FILE] DIR...
  check-mutants-outcome.py --self-test

Each DIR is a cargo-mutants output directory (holding outcomes.json, or
only mutants.json for a shard given no mutant) or a directory holding one
as mutants.out/ — the deep-checks artifact layout, next to the shard's
mutants-exit-code.txt, which an empty shard must also carry, reading 0.

Exit 0 green · 1 red · 2 cannot judge (usage, a malformed dispositions file,
an outcomes.json shape this script does not know). Stdlib only, Python ≥ 3.11.
"""

import argparse
import contextlib
import dataclasses
import io
import json
import os
import pathlib
import re
import sys
import tempfile

try:
    import tomllib
except ModuleNotFoundError:  # Python < 3.11
    print("check-mutants-outcome.py needs Python >= 3.11 (tomllib)", file=sys.stderr)
    sys.exit(2)

ROOT = pathlib.Path(__file__).resolve().parent.parent
DISPOSITIONS = ROOT / ".cargo" / "mutants-dispositions.toml"
POLICY = ROOT / ".cargo" / "mutants.toml"
EXIT_CODE_FILE = "mutants-exit-code.txt"
# cargo-mutants writes the mutants a shard was given here before any build;
# an empty list is its own record that the shard had nothing to test.
MUTANTS_LIST = "mutants.json"

# The outcomes each category may excuse. A timeout-structural entry that
# comes back MISSED is a hang that stopped hanging — a survivor, not a catch.
CATEGORIES = {
    "equivalent": frozenset({"missed"}),
    "timeout-structural": frozenset({"timeout"}),
    "debt": frozenset({"missed", "timeout"}),
}

# outcomes.json `summary` of a mutant scenario (cargo-mutants SummaryOutcome).
# "Success" (only under --check) and "Failure" (unclassified) are no verdict.
BUCKETS = {
    "CaughtMutant": "caught",
    "MissedMutant": "missed",
    "Timeout": "timeout",
    "Unviable": "unviable",
}
COLUMNS = tuple(BUCKETS.values())

# cargo-mutants src/exit_code.rs. 2 and 3 are verdicts for this gate to judge.
EXIT_MEANINGS = {
    0: "success",
    1: "usage error",
    2: "missed mutants",
    3: "timeouts",
    4: "baseline failed",
    5: "filter diff mismatch",
    6: "filter diff invalid",
    70: "internal error",
}
VERDICT_EXIT_CODES = frozenset({0, 2, 3})

NAME_RE = re.compile(r"^(?P<file>[^:]+):(?P<line>\d+):(?P<column>\d+): (?P<mutation>.+)$")
FIELDS = {"file": str, "line": int, "column": int, "mutation": str, "category": str, "reason": str}
# A mutation that names the operator it changes at its column:
# `replace + with * in f`, `delete ! in f`. A replaced body
# (`replace f -> T with v`) or a deleted match arm names none.
OPERATOR_RE = re.compile(r"^(?:replace|delete) (?P<op>\S+)(?: with \S+)? in ")


class Unjudgeable(Exception):
    """Inputs this gate cannot judge (exit 2)."""


@dataclasses.dataclass(frozen=True, order=True)
class Key:
    file: str
    line: int
    column: int
    mutation: str

    def __str__(self) -> str:
        return f"{self.file}:{self.line}:{self.column}: {self.mutation}"


@dataclasses.dataclass(frozen=True)
class Disposition:
    key: Key
    category: str
    reason: str


@dataclasses.dataclass
class Shard:
    label: str
    version: str | None = None
    exit_code: int | None = None
    baseline: str = "absent"
    counts: dict[str, int] = dataclasses.field(default_factory=lambda: dict.fromkeys(COLUMNS, 0))
    outcomes: dict[Key, str] = dataclasses.field(default_factory=dict)
    problems: list[str] = dataclasses.field(default_factory=list)
    # cargo-mutants gave it no mutant (an empty mutants.json, exit 0)
    empty: bool = False


@dataclasses.dataclass(frozen=True)
class Finding:
    key: Key
    outcome: str
    shard: str
    why: str


@dataclasses.dataclass
class Verdict:
    shards: list[Shard]
    covered: dict[Key, Disposition]
    findings: list[Finding]
    stale: list[tuple[Disposition, str]]
    # equivalents the policy also excludes, with the matching pattern
    excluded: list[tuple[Disposition, str]] = dataclasses.field(default_factory=list)

    @property
    def green(self) -> bool:
        return not self.findings and not any(s.problems for s in self.shards)


@dataclasses.dataclass(frozen=True)
class Exclusions:
    """The `exclude_re` patterns of a cargo-mutants policy, and the checkout
    they apply to (the directory holding its `.cargo/`)."""

    patterns: tuple[str, ...]
    root: pathlib.Path

    def matching(self, key: Key) -> str | None:
        """The first pattern that drops this mutant from a run — searched,
        like cargo-mutants, unanchored in its `file:line:col: mutation` name."""
        return next((p for p in self.patterns if re.search(p, str(key))), None)

    def holds(self, key: Key) -> bool:
        """Whether the checkout still holds the anchored mutation: the file
        has the line, and a mutation that names an operator finds it at the
        column (1-based characters, as cargo-mutants prints them). A
        replaced body or a deleted match arm is checked by its line only."""
        try:
            lines = (self.root / key.file).read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeDecodeError):
            return False
        if not 1 <= key.line <= len(lines):
            return False
        operator = OPERATOR_RE.match(key.mutation)
        return operator is None or lines[key.line - 1][key.column - 1 :].startswith(operator["op"])


# ─────────────────────────── inputs ───────────────────────────


def load_dispositions(path: pathlib.Path) -> dict[Key, Disposition]:
    try:
        doc = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as e:
        raise Unjudgeable(f"{path}: {e}") from e
    if set(doc) - {"mutant"}:
        raise Unjudgeable(f"{path}: unknown top-level key(s) {sorted(set(doc) - {'mutant'})}")
    entries = doc.get("mutant", [])
    if not isinstance(entries, list):
        raise Unjudgeable(f"{path}: `mutant` must be an array of tables ([[mutant]])")
    out: dict[Key, Disposition] = {}
    for n, entry in enumerate(entries, 1):
        where = f"{path}: [[mutant]] #{n}"
        if not isinstance(entry, dict):
            raise Unjudgeable(f"{where}: not a table")
        if entry.keys() != FIELDS.keys():
            missing = sorted(FIELDS.keys() - entry.keys())
            unknown = sorted(entry.keys() - FIELDS.keys())
            raise Unjudgeable(f"{where}: missing {missing} / unknown {unknown} field(s)")
        for field, kind in FIELDS.items():
            value = entry[field]
            # type(), not isinstance(): TOML booleans must not pass as ints
            ok = type(value) is kind and (value >= 1 if kind is int else bool(value.strip()))
            if not ok:
                wanted = "a positive int" if kind is int else "a non-empty string"
                raise Unjudgeable(f"{where}: `{field}` must be {wanted}")
        if entry["category"] not in CATEGORIES:
            known = ", ".join(CATEGORIES)
            raise Unjudgeable(f"{where}: category {entry['category']!r} is not one of {known}")
        key = Key(entry["file"], entry["line"], entry["column"], entry["mutation"])
        if key in out:
            raise Unjudgeable(f"{where}: second disposition for {key}")
        out[key] = Disposition(key, entry["category"], entry["reason"])
    return out


def load_exclusions(path: pathlib.Path) -> Exclusions:
    """The `exclude_re` list of a cargo-mutants policy file; the checkout it
    applies to is the directory holding the file's `.cargo/`."""
    try:
        doc = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as e:
        raise Unjudgeable(f"{path}: {e}") from e
    patterns = doc.get("exclude_re", [])
    if not isinstance(patterns, list) or not all(isinstance(p, str) for p in patterns):
        raise Unjudgeable(f"{path}: `exclude_re` must be an array of strings")
    for pattern in patterns:
        try:
            re.compile(pattern)
        except re.error as e:
            raise Unjudgeable(f"{path}: exclude_re {pattern!r}: {e}") from e
    return Exclusions(tuple(patterns), path.resolve().parent.parent)


def mutant_listing(path: pathlib.Path) -> list | None:
    """cargo-mutants' list of the mutants a shard was given, or None when it
    is absent, unreadable or not a list."""
    try:
        doc = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, ValueError):
        return None
    return doc if isinstance(doc, list) else None


def mutant_key(mutant: object) -> Key:
    name = mutant.get("name") if isinstance(mutant, dict) else None
    match = NAME_RE.match(name) if isinstance(name, str) else None
    if match is None:
        raise Unjudgeable(f"mutant without a `file:line:col: mutation` name: {str(mutant)[:200]}")
    return Key(match["file"], int(match["line"]), int(match["column"]), match["mutation"])


def load_shard(label: str) -> Shard:
    shard = Shard(label)
    root = pathlib.Path(label)
    if not root.is_dir():
        shard.problems.append("no artifact — the shard died, timed out before its upload, or never ran")
        return shard
    own = any((root / name).is_file() for name in ("outcomes.json", MUTANTS_LIST))
    out = root if own else root / "mutants.out"
    marker = root / EXIT_CODE_FILE
    if marker.is_file():
        text = marker.read_text(encoding="utf-8", errors="replace").strip()
        if not text.isdigit():
            shard.problems.append(f"{EXIT_CODE_FILE} holds {text!r}, not an exit code")
        else:
            shard.exit_code = int(text)
            if shard.exit_code not in VERDICT_EXIT_CODES:
                meaning = EXIT_MEANINGS.get(shard.exit_code, "unknown")
                shard.problems.append(
                    f"cargo-mutants exited {shard.exit_code} ({meaning}) — a tool-level failure, not a mutant verdict"
                )
    path = out / "outcomes.json"
    try:
        doc = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        empty_or_problem(shard, mutant_listing(out / MUTANTS_LIST), artifact=out != root)
        return shard
    except (OSError, UnicodeDecodeError, ValueError) as e:
        shard.problems.append(f"unreadable outcomes.json ({e})")
        return shard
    if not isinstance(doc, dict) or not isinstance(doc.get("outcomes"), list):
        raise Unjudgeable(f"{path}: no `outcomes` list — an outcomes.json shape this script does not know")
    shard.version = doc.get("cargo_mutants_version")
    if doc.get("end_time") is None:
        shard.problems.append("did not finish (outcomes.json has no end_time) — killed mid-run, its verdict is partial")
    for entry in doc["outcomes"]:
        scenario = entry.get("scenario") if isinstance(entry, dict) else None
        summary = entry.get("summary") if isinstance(entry, dict) else None
        if scenario == "Baseline" or (isinstance(scenario, dict) and "Baseline" in scenario):
            shard.baseline = "ok" if summary == "Success" else str(summary)
            if summary != "Success":
                shard.problems.append(
                    f"baseline {summary} — the unmutated suite already fails, no mutant verdict means anything"
                )
            continue
        if not isinstance(scenario, dict) or "Mutant" not in scenario:
            raise Unjudgeable(f"{path}: unknown scenario {str(scenario)[:120]}")
        key = mutant_key(scenario["Mutant"])
        bucket = BUCKETS.get(summary)
        if bucket is None:
            shard.problems.append(f"{key}: outcome {summary!r} is neither caught, missed, timeout nor unviable")
            continue
        if key in shard.outcomes:
            shard.problems.append(f"{key}: tested twice in this shard")
        shard.outcomes[key] = bucket
        shard.counts[bucket] += 1
    if any(shard.counts.values()) and shard.baseline == "absent":
        shard.problems.append("mutants were tested but no baseline outcome was recorded")
    for bucket, listed in shard.counts.items():
        recorded = doc.get(bucket)
        if isinstance(recorded, int) and recorded != listed:
            shard.problems.append(f"outcomes.json counts {bucket} = {recorded} but lists {listed}")
    return shard


def empty_or_problem(shard: Shard, listing: list | None, *, artifact: bool) -> None:
    """A shard without outcomes.json: empty when cargo-mutants listed no
    mutant for it and exited 0 (it returns before any build), a problem
    otherwise. In the artifact layout the recorded exit code is required:
    without it nothing shows that cargo-mutants finished."""
    if listing != []:
        shard.problems.append("no outcomes.json — cargo-mutants never started testing")
    elif shard.exit_code == 0 or (shard.exit_code is None and not artifact):
        shard.empty, shard.baseline = True, "none"
    elif shard.exit_code is None:
        shard.problems.append(f"{MUTANTS_LIST} lists no mutant, but no {EXIT_CODE_FILE} shows that cargo-mutants finished")
    elif shard.exit_code in VERDICT_EXIT_CODES:
        meaning = EXIT_MEANINGS[shard.exit_code]
        shard.problems.append(f"{MUTANTS_LIST} lists no mutant, yet cargo-mutants exited {shard.exit_code} ({meaning})")
    # any other exit code is already a problem: a tool-level failure


# ─────────────────────────── the gate ───────────────────────────


def judge(
    shards: list[Shard], dispositions: dict[Key, Disposition], exclusions: Exclusions | None = None
) -> Verdict:
    seen: dict[Key, tuple[str, str]] = {}
    for shard in shards:
        for key, bucket in shard.outcomes.items():
            if key in seen:
                shard.problems.append(f"{key}: also tested by {seen[key][1]} — overlapping shard inputs")
            seen[key] = (bucket, shard.label)
    covered: dict[Key, Disposition] = {}
    findings: list[Finding] = []
    for key, (bucket, label) in sorted(seen.items()):
        if bucket not in ("missed", "timeout"):
            continue
        disposition = dispositions.get(key)
        if disposition is None:
            findings.append(Finding(key, bucket, label, drift_hint(key, dispositions, seen)))
        elif bucket not in CATEGORIES[disposition.category]:
            why = f"dispositioned `{disposition.category}`, which does not cover a {bucket.upper()}"
            findings.append(Finding(key, bucket, label, why))
        else:
            covered[key] = disposition
    fates = {"caught": "now CAUGHT — delete the entry", "unviable": "now UNVIABLE"}
    # entry → the undispositioned mutant it most likely became
    drifted = {k: f.key for f in findings for k in drift_candidates(f.key, dispositions, seen)}
    stale: list[tuple[Disposition, str]] = []
    excluded: list[tuple[Disposition, str]] = []
    for key, disposition in sorted(dispositions.items()):
        if key in seen:
            if seen[key][0] in fates:
                stale.append((disposition, fates[seen[key][0]]))
            continue
        if exclusions is None:
            stale.append((disposition, "not generated by these shards (excluded, moved or removed)"))
            continue
        pattern = exclusions.matching(key)
        if pattern is None:
            stale.append((disposition, "not generated and not excluded — its code moved or is gone"))
        elif disposition.category != "equivalent":
            why = f"excluded by {pattern!r}, but only an equivalent may be: this `{disposition.category}` mutant goes unmeasured"
            stale.append((disposition, why))
        elif key in drifted:
            moved = drifted[key]
            why = f"excluded, but the same mutation came back undispositioned at {moved.line}:{moved.column} — re-anchor the exclusion and the entry"
            stale.append((disposition, why))
        elif not exclusions.holds(key):
            stale.append((disposition, "excluded, but the checkout no longer holds it there — its code moved or is gone"))
        else:
            excluded.append((disposition, pattern))
    return Verdict(shards, covered, findings, stale, excluded)


def drift_candidates(key: Key, dispositions: dict[Key, Disposition], seen: dict[Key, tuple[str, str]]) -> list[Key]:
    """Entries on the same file + mutation text whose anchor matched nothing
    here: most likely the entry `key` was, moved by an edit above it."""
    return sorted(
        d.key
        for d in dispositions.values()
        if d.key.file == key.file and d.key.mutation == key.mutation and d.key not in seen
    )


def drift_hint(key: Key, dispositions: dict[Key, Disposition], seen: dict[Key, tuple[str, str]]) -> str:
    """The stale entry an undispositioned mutant probably was. Never applied,
    only named."""
    moved = drift_candidates(key, dispositions, seen)
    if not moved:
        return "no disposition"
    anchors = ", ".join(f"{k.line}:{k.column}" for k in moved)
    return f"no disposition — probably the stale entry at {anchors} (same file and mutation): re-verify, then re-anchor"


# ─────────────────────────── reports ───────────────────────────


def category_counts(verdict: Verdict) -> str:
    counts = {c: 0 for c in CATEGORIES}
    for d in verdict.covered.values():
        counts[d.category] += 1
    return " · ".join(f"{c} {n}" for c, n in counts.items())


def headline(verdict: Verdict) -> str:
    if verdict.green:
        return "PASS — every missed or timed-out mutant is dispositioned"
    problems = sum(len(s.problems) for s in verdict.shards)
    return f"FAIL — {len(verdict.findings)} undispositioned mutant(s) · {problems} shard problem(s)"


def render_text(verdict: Verdict) -> str:
    versions = sorted({s.version for s in verdict.shards if s.version})
    width = max([len("shard")] + [len(s.label) for s in verdict.shards])
    lines = [f"mutation verdict · {len(verdict.shards)} shard(s) · cargo-mutants {', '.join(versions) or 'unknown'}"]
    lines.append(f"  {'shard':<{width}}  {'baseline':<8}" + "".join(f"{c:>10}" for c in COLUMNS) + f"{'exit':>6}")
    for s in verdict.shards:
        exit_code = "-" if s.exit_code is None else str(s.exit_code)
        row = "".join(f"{s.counts[c]:>10}" for c in COLUMNS)
        lines.append(f"  {s.label:<{width}}  {s.baseline:<8}{row}{exit_code:>6}")
    totals = "".join(f"{sum(s.counts[c] for s in verdict.shards):>10}" for c in COLUMNS)
    lines.append(f"  {'total':<{width}}  {'':<8}{totals}")
    lines.append(f"dispositioned {len(verdict.covered)}: {category_counts(verdict)}")
    empty = [s.label for s in verdict.shards if s.empty]
    if empty:
        lines.append(f"empty shards {len(empty)} (cargo-mutants listed no mutant, exit 0): {', '.join(empty)}")
    for s in verdict.shards:
        for problem in s.problems:
            lines.append(f"SHARD PROBLEM  {s.label}: {problem}")
    for f in verdict.findings:
        lines.append(f"UNDISPOSITIONED {f.outcome.upper():<7} {f.key}  [{f.shard}]")
        lines.append(f"    {f.why}")
    if verdict.excluded:
        lines.append(f"excluded and documented {len(verdict.excluded)} (equivalents the policy also excludes, never generated):")
        lines.extend(f"  {d.category:<18} {d.key} — exclude_re {p!r}" for d, p in verdict.excluded)
    if verdict.stale:
        lines.append(f"stale dispositions {len(verdict.stale)} (listed, never failed):")
        lines.extend(f"  {d.category:<18} {d.key} — {fate}" for d, fate in verdict.stale)
    lines.append(f"VERDICT {headline(verdict)}")
    return "\n".join(lines)


def md(text: object) -> str:
    """A table-safe code span (GFM splits cells on `|`, even inside backticks)."""
    return "`" + str(text).replace("|", "\\|") + "`"


def render_markdown(verdict: Verdict) -> str:
    lines = [f"### Mutation testing — {headline(verdict)}", ""]
    lines.append("| shard | baseline | " + " | ".join(COLUMNS) + " | exit |")
    lines.append("|---|---|" + "--:|" * len(COLUMNS) + "--:|")
    for s in verdict.shards:
        exit_code = "—" if s.exit_code is None else str(s.exit_code)
        counts = " | ".join(str(s.counts[c]) for c in COLUMNS)
        lines.append(f"| {md(s.label)} | {s.baseline} | {counts} | {exit_code} |")
    totals = " | ".join(f"**{sum(s.counts[c] for s in verdict.shards)}**" for c in COLUMNS)
    lines.append(f"| **total** | | {totals} | |")
    lines += ["", f"Dispositioned: {len(verdict.covered)} — {category_counts(verdict)}."]
    empty = [md(s.label) for s in verdict.shards if s.empty]
    if empty:
        lines += ["", f"Empty shards ({len(empty)}, cargo-mutants listed no mutant, exit 0): {', '.join(empty)}."]
    problems = [(s.label, p) for s in verdict.shards for p in s.problems]
    if problems:
        lines += ["", "**Shard problems**", ""] + [f"- {md(label)}: {p}" for label, p in problems]
    if verdict.findings:
        lines += ["", f"**Undispositioned ({len(verdict.findings)})**", ""]
        lines += ["| outcome | mutant | shard | note |", "|---|---|---|---|"]
        for f in verdict.findings:
            lines.append(f"| {f.outcome.upper()} | {md(f.key)} | {md(f.shard)} | {f.why} |")
    if verdict.excluded:
        lines += ["", f"<details><summary>Excluded and documented ({len(verdict.excluded)})</summary>", ""]
        lines += ["| category | anchor | exclude_re |", "|---|---|---|"]
        lines += [f"| {d.category} | {md(d.key)} | {md(p)} |" for d, p in verdict.excluded]
        lines += ["", "</details>"]
    if verdict.stale:
        lines += ["", f"<details><summary>Stale dispositions ({len(verdict.stale)})</summary>", ""]
        lines += ["| category | anchor | fate |", "|---|---|---|"]
        lines += [f"| {d.category} | {md(d.key)} | {fate} |" for d, fate in verdict.stale]
        lines += ["", "</details>"]
    return "\n".join(lines) + "\n\n"


def escape(text: str, prop: bool = False) -> str:
    text = str(text).replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
    return text.replace(":", "%3A").replace(",", "%2C") if prop else text


def render_annotations(verdict: Verdict) -> str:
    lines = []
    for s in verdict.shards:
        lines += [f"::error title=mutants shard::{escape(s.label)}: {escape(p)}" for p in s.problems]
    for f in verdict.findings:
        where = f"file={escape(f.key.file, True)},line={f.key.line},col={f.key.column}"
        title = escape(f"undispositioned {f.outcome.upper()} mutant", True)
        lines.append(f"::error {where},title={title}::{escape(f.key.mutation)} — {escape(f.why)}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__.split("\n", 1)[0], epilog="Exit 0 green · 1 red · 2 cannot judge."
    )
    parser.add_argument("--dispositions", type=pathlib.Path, help="the versioned dispositions file")
    parser.add_argument(
        "--exclusions",
        type=pathlib.Path,
        help="the cargo-mutants policy (.cargo/mutants.toml): an equivalent it also excludes is excluded and documented",
    )
    parser.add_argument("--markdown", type=pathlib.Path, help="append a Markdown report (e.g. $GITHUB_STEP_SUMMARY)")
    parser.add_argument("--self-test", action="store_true", help="run the built-in scenarios, then exit")
    parser.add_argument("dirs", nargs="*", metavar="DIR", help="one cargo-mutants output directory per shard")
    args = parser.parse_args(argv)
    if args.self_test:
        if args.dirs or args.dispositions or args.exclusions or args.markdown:
            parser.error("--self-test takes no other argument")
        return self_test()
    if args.dispositions is None or not args.dirs:
        parser.error("--dispositions and at least one DIR are required")
    try:
        dispositions = load_dispositions(args.dispositions)
        exclusions = load_exclusions(args.exclusions) if args.exclusions else None
        shards = [load_shard(d) for d in args.dirs]
    except Unjudgeable as e:
        print(f"cannot judge: {e}", file=sys.stderr)
        return 2
    verdict = judge(shards, dispositions, exclusions)
    print(render_text(verdict))
    if os.environ.get("GITHUB_ACTIONS") == "true" and not verdict.green:
        print(render_annotations(verdict))
    if args.markdown:
        with args.markdown.open("a", encoding="utf-8") as f:
            f.write(render_markdown(verdict))
    return 0 if verdict.green else 1


# ─────────────────────────── self-test ───────────────────────────

GS1 = "crates/qrcode-ai-scanner/src/gs1.rs"
EQUIVALENT = f"{GS1}:227:15: replace > with >= in split_head"
HANG = f"{GS1}:228:13: replace -= with /= in split_head"
DEBT = "crates/qrcode-ai-scanner/src/input.rs:120:14: replace > with >= in validate_dims"
NEW_MISS = f"{GS1}:266:43: replace + with * in parse_element_string"
SEEDED = f"""
[[mutant]]
file = "{GS1}"
line = 227
column = 15
mutation = "replace > with >= in split_head"
category = "equivalent"
reason = "cut is usize and is_char_boundary(0) is true: both guards stop at the same index"

[[mutant]]
file = "{GS1}"
line = 228
column = 13
mutation = "replace -= with /= in split_head"
category = "timeout-structural"
reason = "cut /= 1 freezes the down-walk into a spin"

[[mutant]]
file = "crates/qrcode-ai-scanner/src/input.rs"
line = 120
column = 14
mutation = "replace > with >= in validate_dims"
category = "debt"
reason = "surviving, to be killed by a test"

[[mutant]]
file = "crates/qrcode-ai-scanner/src/rescue/bitstream.rs"
line = 144
column = 53
mutation = "replace | with ^ in parse"
category = "equivalent"
reason = "bit-disjoint operands"
"""


def _outcome(name: str | None, summary: str) -> dict:
    scenario = "Baseline" if name is None else {"Mutant": {"name": name, "file": name.split(":", 1)[0]}}
    return {"scenario": scenario, "summary": summary, "log_path": "log/x.log", "diff_path": None, "phase_results": []}


def _shard(base: pathlib.Path, name: str, mutants: list[tuple[str, str]], *, baseline: str = "Success",
           finished: bool = True, exit_code: int | None = None, counts: dict[str, int] | None = None) -> str:
    """One shard in the deep-checks artifact layout: NAME/mutants.out/outcomes.json."""
    out = base / name / "mutants.out"
    out.mkdir(parents=True)
    outcomes = [_outcome(None, baseline)] + [_outcome(n, summary) for n, summary in mutants]
    doc = {
        "outcomes": outcomes,
        "total_mutants": len(mutants),
        "start_time": "2026-10-05T04:09:39Z",
        "end_time": "2026-10-05T05:08:29Z" if finished else None,
        "cargo_mutants_version": "27.1.0",
    }
    tally = {b: sum(1 for _, s in mutants if BUCKETS.get(s) == b) for b in COLUMNS}
    doc.update(counts or tally)
    (out / "outcomes.json").write_text(json.dumps(doc), encoding="utf-8")
    if exit_code is not None:
        (base / name / EXIT_CODE_FILE).write_text(f"{exit_code}\n", encoding="utf-8")
    return str(base / name)


def _empty_shard(base: pathlib.Path, name: str, *, exit_code: int | None = 0, listing: str = "[]") -> str:
    """A shard as deep-checks uploads it when cargo-mutants was given no
    mutant: mutants.json, debug.log, no outcomes.json (it returns before the
    baseline)."""
    out = base / name / "mutants.out"
    out.mkdir(parents=True)
    (out / MUTANTS_LIST).write_text(listing, encoding="utf-8")
    (out / "debug.log").write_text("WARN cargo_mutants::lab: No mutants found under the active filters\n", encoding="utf-8")
    if exit_code is not None:
        (base / name / EXIT_CODE_FILE).write_text(f"{exit_code}\n", encoding="utf-8")
    return str(base / name)


def _checkout(base: pathlib.Path, name: str, policy: list[str], sources: dict[str, dict[int, str]]) -> str:
    """A checkout holding a policy (`.cargo/mutants.toml`) and source files
    whose given lines are set (the others filler); returns the policy path."""
    root = base / name
    (root / ".cargo").mkdir(parents=True)
    (root / ".cargo" / "mutants.toml").write_text(f"exclude_re = {json.dumps(policy)}\n", encoding="utf-8")
    for file, lines in sources.items():
        text = [lines.get(n, "// filler") for n in range(1, max(lines) + 1)]
        (root / file).parent.mkdir(parents=True, exist_ok=True)
        (root / file).write_text("\n".join(text) + "\n", encoding="utf-8")
    return str(root / ".cargo" / "mutants.toml")


def _run(argv: list[str]) -> tuple[int, str]:
    sink = io.StringIO()
    env = os.environ.pop("GITHUB_ACTIONS", None)  # keep scenario annotations out of the real log
    try:
        with contextlib.redirect_stdout(sink), contextlib.redirect_stderr(sink):
            try:
                code = main(argv)
            except SystemExit as e:  # argparse usage errors
                code = e.code if isinstance(e.code, int) else 2
    finally:
        if env is not None:
            os.environ["GITHUB_ACTIONS"] = env
    return code, sink.getvalue()


def self_test() -> int:
    failures: list[str] = []
    ran = 0

    def expect(name: str, argv: list[str], code: int, *needles: str, absent: tuple[str, ...] = ()) -> None:
        nonlocal ran
        ran += 1
        got, out = _run(argv)
        missing = [n for n in needles if n not in out]
        present = [a for a in absent if a in out]
        if got != code or missing or present:
            failures.append(f"{name}: exit {got} (want {code}) missing {missing} unexpected {present}\n{out}")

    with tempfile.TemporaryDirectory(prefix="mutants-selftest-") as tmp:
        base = pathlib.Path(tmp)
        disp = base / "dispositions.toml"
        disp.write_text(SEEDED, encoding="utf-8")
        d = ["--dispositions", str(disp)]
        ok = [(EQUIVALENT, "MissedMutant"), (HANG, "Timeout"), (DEBT, "MissedMutant"),
              ("crates/x.rs:1:1: replace f -> bool with true", "CaughtMutant"),
              ("crates/x.rs:2:1: replace g -> bool with true", "Unviable")]
        green = _shard(base, "green", ok, exit_code=3)
        report = base / "summary.md"
        expect("seeded run is green", d + ["--markdown", str(report), green], 0,
               "VERDICT PASS", "equivalent 1 · timeout-structural 1 · debt 1",
               "stale dispositions 1", "bitstream.rs:144:53")
        md_text = report.read_text(encoding="utf-8") if report.exists() else ""
        if "### Mutation testing — PASS" not in md_text or "replace \\| with ^ in parse" not in md_text:
            failures.append(f"markdown report: header or escaped `|` missing\n{md_text}")
        ran += 1

        injected = _shard(base, "injected", ok + [(NEW_MISS, "MissedMutant")], exit_code=3)
        expect("an injected new miss fails", d + [injected], 1,
               "UNDISPOSITIONED MISSED  " + NEW_MISS, "VERDICT FAIL — 1 undispositioned")
        moved = EQUIVALENT.replace(":227:15:", ":231:15:")
        drifted = _shard(base, "drifted", [(moved, "MissedMutant"), (HANG, "Timeout"), (DEBT, "MissedMutant")])
        expect("a drifted anchor fails and names the stale entry", d + [drifted], 1,
               "probably the stale entry at 227:15")
        stopped = _shard(base, "stopped", [(HANG, "MissedMutant")])
        expect("a hang that stops hanging is a survivor", d + [stopped], 1,
               "dispositioned `timeout-structural`, which does not cover a MISSED")
        slow = _shard(base, "slow", [(EQUIVALENT, "Timeout")])
        expect("an equivalent never excuses a timeout", d + [slow], 1,
               "dispositioned `equivalent`, which does not cover a TIMEOUT")
        caught = _shard(base, "caught", [(DEBT, "CaughtMutant")])
        expect("killed debt turns stale, not red", d + [caught], 0, "now CAUGHT — delete the entry")
        red_base = _shard(base, "red-baseline", [], baseline="Failure", exit_code=4)
        expect("a baseline failure fails", d + [red_base], 1, "baseline Failure", "exited 4 (baseline failed)")
        partial = _shard(base, "partial", [(DEBT, "MissedMutant")], finished=False)
        expect("an unfinished shard fails", d + [partial], 1, "did not finish")
        crashed = _shard(base, "crashed", [(DEBT, "MissedMutant")], exit_code=70)
        expect("a tool-level exit code fails", d + [crashed], 1, "exited 70 (internal error)")
        expect("a missing shard fails", d + [green, str(base / "absent")], 1, "no artifact")
        expect("overlapping shards fail", d + [green, green], 1, "overlapping shard inputs")
        lying = _shard(base, "lying", [(DEBT, "MissedMutant")], counts={"missed": 2})
        expect("counters that disagree with the list fail", d + [lying], 1, "counts missed = 2 but lists 1")
        odd = _shard(base, "odd", [(DEBT, "Failure")])
        expect("an unclassified mutant outcome fails", d + [odd], 1, "is neither caught, missed")
        bare = base / "bare"
        bare.mkdir()
        expect("a shard without outcomes.json fails", d + [str(bare)], 1, "no outcomes.json")

        for name, text, needle in (
            ("duplicate entry", SEEDED + SEEDED.split("\n\n")[0], "second disposition"),
            ("unknown category", SEEDED.replace('"debt"', '"tolerated"'), "is not one of"),
            ("missing reason", SEEDED.replace('reason = "bit-disjoint operands"', ""), "missing ['reason']"),
            ("boolean line", SEEDED.replace("line = 120", "line = true"), "`line` must be"),
        ):
            bad = base / f"{name.replace(' ', '-')}.toml"
            bad.write_text(text, encoding="utf-8")
            expect(f"malformed dispositions: {name}", ["--dispositions", str(bad), green], 2, needle)
        expect("usage without shard dirs", d, 2, "at least one DIR")

        # A shard cargo-mutants gave no mutant (fewer mutants than shards):
        # its own empty mutants.json and exit 0 prove it, nothing else does.
        expect("a shard given no mutant is empty, not a problem", d + [green, _empty_shard(base, "empty")], 0,
               "VERDICT PASS", "empty shards 1", absent=("SHARD PROBLEM",))
        plain = base / "plain-empty"
        plain.mkdir()
        (plain / MUTANTS_LIST).write_text("[]", encoding="utf-8")
        expect("a plain output directory given no mutant is empty", d + [green, str(plain)], 0, "empty shards 1")
        expect("an empty list without a recorded exit code fails",
               d + [_empty_shard(base, "empty-unrecorded", exit_code=None)], 1,
               "lists no mutant, but no mutants-exit-code.txt")
        expect("an empty list with a verdict exit code fails", d + [_empty_shard(base, "empty-exit-2", exit_code=2)], 1,
               "lists no mutant, yet cargo-mutants exited 2")
        expect("an empty list with a tool-level exit code fails",
               d + [_empty_shard(base, "empty-exit-70", exit_code=70)], 1, "exited 70 (internal error)")
        given = json.dumps([{"name": "crates/x.rs:1:1: replace f -> bool with true"}])
        expect("a shard given mutants that never tested them fails", d + [_empty_shard(base, "untested", listing=given)],
               1, "no outcomes.json — cargo-mutants never started testing", absent=("empty shards",))
        expect("an unreadable mutants.json proves nothing", d + [_empty_shard(base, "torn", listing="[")], 1,
               "no outcomes.json", absent=("empty shards",))

        # An equivalent the policy also excludes is never generated: excluded
        # and documented while the checkout still holds it, stale otherwise.
        bitstream = "crates/qrcode-ai-scanner/src/rescue/bitstream.rs"
        sources = {
            GS1: {227: "    while cut > 0 && !s.is_char_boundary(cut) {"},
            bitstream: {144: " " * 52 + "| (v % split)"},
            "crates/qrcode-ai-scanner/src/input.rs": {120: "    if width > MAX {"},
        }
        policy = [
            r"gs1\.rs:227:15: replace > with >= in split_head",
            r"bitstream\.rs:144:53: replace \| with \^ in parse",
            r"input\.rs:120:14: replace > with >= in validate_dims",
        ]
        held = _checkout(base, "checkout", policy, sources)
        quiet = _shard(base, "quiet", [(HANG, "Timeout"), (DEBT, "MissedMutant")], exit_code=3)
        expect("an excluded equivalent is documented, not stale", d + ["--exclusions", held, quiet], 0,
               "VERDICT PASS", "excluded and documented 2", absent=("stale dispositions",))
        report = base / "excluded.md"
        _run(d + ["--exclusions", held, "--markdown", str(report), quiet])
        md_text = report.read_text(encoding="utf-8") if report.exists() else ""
        if "Excluded and documented (2)" not in md_text or "Stale dispositions" in md_text:
            failures.append(f"markdown report: excluded block missing or stale block present\n{md_text}")
        ran += 1
        expect("without the policy the same entries stay stale", d + [quiet], 0,
               "stale dispositions 2", absent=("excluded and documented",))
        moved = {**sources, GS1: {227: "    while cut != 0 && !s.is_char_boundary(cut) {"}}
        gone = _checkout(base, "edited", policy, moved)
        expect("an excluded equivalent whose code changed stays stale", d + ["--exclusions", gone, quiet], 0,
               "excluded and documented 1", "the checkout no longer holds it there")
        partial = _checkout(base, "unexcluded", policy[:1] + policy[2:], sources)
        expect("an equivalent no exclusion matches stays stale", d + ["--exclusions", partial, quiet], 0,
               "bitstream.rs:144:53: replace | with ^ in parse — not generated and not excluded")
        unmeasured = _shard(base, "unmeasured", [(EQUIVALENT, "MissedMutant"), (HANG, "Timeout")], exit_code=3)
        expect("only an equivalent may be documented by an exclusion", d + ["--exclusions", held, unmeasured], 0,
               "but only an equivalent may be: this `debt` mutant goes unmeasured")
        drifted = _shard(base, "drifted-out", [(EQUIVALENT.replace(":227:15:", ":231:15:"), "MissedMutant")])
        expect("an excluded equivalent that came back elsewhere stays stale", d + ["--exclusions", held, drifted], 1,
               "came back undispositioned at 231:15", "probably the stale entry at 227:15")
        killed = _shard(base, "killed", [(EQUIVALENT, "CaughtMutant")])
        expect("an entry the run reports caught stays stale", d + ["--exclusions", held, killed], 0,
               "split_head — now CAUGHT")
        for name, text, needle in (
            ("not a list", 'exclude_re = "x"\n', "must be an array of strings"),
            ("bad pattern", 'exclude_re = ["("]\n', "exclude_re '('"),
        ):
            bad = base / f"policy-{name.replace(' ', '-')}.toml"
            bad.write_text(text, encoding="utf-8")
            expect(f"malformed policy: {name}", d + ["--exclusions", str(bad), green], 2, needle)

        # The versioned policy drops both shapes of the cfg(fuzzing) bridge —
        # a function replaced and an operator inside one — and nothing else.
        if POLICY.is_file():
            versioned = load_exclusions(POLICY)
            core = "crates/qrcode-ai-scanner/src"
            bridge = [
                f"{core}/lib.rs:70:9: replace fuzzing::classify_text with ()",
                f"{core}/rescue/mod.rs:474:9: replace fuzz_api::parse_bitstream with ()",
                f"{core}/rescue/mod.rs:489:32: replace + with * in fuzz_api::parse_bitstream",
                f"{core}/rescue/mod.rs:494:38: replace == with != in fuzz_api::parse_bitstream",
                f"{core}/rescue/mod.rs:499:45: replace % with / in fuzz_api::parse_bitstream",
                f"{core}/lib.rs:86:17: delete ! in fuzzing::correct_block",
            ]
            outside = [
                f"{core}/rescue/bitstream.rs:299:32: replace + with * in parse",
                f"{core}/rescue/mod.rs:120:9: replace == with != in rescue_candidates",
                f"{core}/payload.rs:40:5: replace classify -> Payload with Default::default()",
            ]
            kept = [n for n in bridge if versioned.matching(mutant_key({"name": n})) is None]
            dropped = [n for n in outside if versioned.matching(mutant_key({"name": n})) is not None]
            if kept or dropped:
                failures.append(f"versioned policy: bridge mutants kept {kept}, others dropped {dropped}")
            ran += 1

        # The versioned file judges its own anchors green — each replayed as the
        # outcome its category covers — and an injected miss red against it.
        if DISPOSITIONS.is_file():
            real = load_dispositions(DISPOSITIONS)
            replay = [(str(k), "Timeout" if v.category == "timeout-structural" else "MissedMutant")
                      for k, v in real.items()]
            own = _shard(base, "own-anchors", replay)
            expect("the versioned dispositions cover their own anchors", ["--dispositions", str(DISPOSITIONS), own],
                   0, "VERDICT PASS", f"dispositioned {len(real)}")
            own_plus = _shard(base, "own-plus-one", replay + [(NEW_MISS + " (injected)", "MissedMutant")])
            expect("the versioned dispositions reject an injected miss",
                   ["--dispositions", str(DISPOSITIONS), own_plus], 1, "VERDICT FAIL — 1 undispositioned")

    for failure in failures:
        print(f"SELF-TEST FAILED · {failure}", file=sys.stderr)
    print(f"self-test: {ran - len(failures)}/{ran} scenarios passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
