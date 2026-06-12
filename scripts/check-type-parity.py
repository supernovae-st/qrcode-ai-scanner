#!/usr/bin/env python3
"""Cross-surface type-parity gate — Rust enums ↔ TS unions ↔ JSON Schema.

Exit 1 on ANY divergence. Surfaces compared:
  payload kinds : payload.rs · report-types.d.ts · schema $defs.Payload
  hints         : report.rs  · report-types.d.ts · schema $defs.Hint
  engines       : report.rs  · report-types.d.ts · schema $defs.EngineKind
Schema leniency is deliberate (additive evolution): the tag property stays
`type: string`, but every FIELDED variant must have a conditional, and the
fieldless set must be a subset of the documented union.
"""

import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


def snake(name: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def rust_variants(path: str, enum_name: str) -> set[str]:
    src = (ROOT / path).read_text()
    start = src.index(f"pub enum {enum_name}")
    depth = 0
    body_start = src.index("{", start)
    for i in range(body_start, len(src)):
        if src[i] == "{":
            depth += 1
        elif src[i] == "}":
            depth -= 1
            if depth == 0:
                body = src[body_start : i + 1]
                break
    # variant = identifier at indent-4 followed by `,` or `{` (skip nested fields)
    names = re.findall(r"^\s{4}([A-Z]\w*)\s*[,{]", body, re.M)
    return {snake(n) for n in names}


def ts_tags(pattern: str) -> set[str]:
    src = (ROOT / "bindings/report-types.d.ts").read_text()
    return set(re.findall(pattern, src))


def schema() -> dict:
    return json.loads((ROOT / "spec/scan-report.schema.json").read_text())


def fail(label: str, diff: set[str]) -> None:
    print(f"DRIFT [{label}]: {sorted(diff)}", file=sys.stderr)
    sys.exit(1)


def main() -> None:
    doc = schema()

    # ---- payload kinds ----
    rust = rust_variants("crates/qrcode-ai-scanner/src/payload.rs", "Payload")
    ts = ts_tags(r'kind: "(\w+)"')
    if rust != ts:
        fail("payload rust↔ts", rust ^ ts)
    conds: set[str] = set()
    for cond in doc["$defs"]["Payload"]["allOf"]:
        kind = cond["if"]["properties"]["kind"]
        conds.update([kind["const"]] if "const" in kind else kind["enum"])
    fieldless = {"text"}  # the only kind with no fields at all
    if rust - conds - fieldless:
        fail("payload kinds missing a schema conditional", rust - conds - fieldless)
    if conds - rust:
        fail("schema conditionals for unknown kinds", conds - rust)

    # ---- hints ----
    rust_h = rust_variants("crates/qrcode-ai-scanner/src/report.rs", "Hint")
    ts_h = ts_tags(r'hint: "(\w+)"')
    if rust_h != ts_h:
        fail("hint rust↔ts", rust_h ^ ts_h)
    hint_conds: set[str] = set()
    for cond in doc["$defs"]["Hint"]["allOf"]:
        tag = cond["if"]["properties"]["hint"]
        hint_conds.update([tag["const"]] if "const" in tag else tag["enum"])
    fielded = {"raise_error_correction", "fix_finder_pattern", "low_correction_margin"}
    if fielded - hint_conds:
        fail("fielded hints missing a schema conditional", fielded - hint_conds)

    # ---- engines + flat enums ----
    for enum_name, ts_pat, def_name in [
        ("EngineKind", r'EngineKind = ([^;]+);', "EngineKind"),
        ("Charset", r'Charset = ([^;]+);', "Charset"),
        ("Grade", r'\bGrade = ("excellent[^;]+);', "Grade"),
        ("StressAxis", r'StressAxis = ([^;]+);', "StressAxis"),
    ]:
        rust_e = rust_variants("crates/qrcode-ai-scanner/src/report.rs", enum_name)
        src = (ROOT / "bindings/report-types.d.ts").read_text()
        m = re.search(ts_pat, src)
        ts_e = set(re.findall(r'"(\w+)"', m.group(1))) if m else set()
        schema_e = set(doc["$defs"][def_name]["enum"])
        if rust_e != ts_e:
            fail(f"{enum_name} rust↔ts", rust_e ^ ts_e)
        if rust_e != schema_e:
            fail(f"{enum_name} rust↔schema", rust_e ^ schema_e)

    print("type parity OK: payload kinds · hints · engines · charsets · grades · axes")


if __name__ == "__main__":
    main()
