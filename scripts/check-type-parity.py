#!/usr/bin/env python3
"""Cross-surface type-parity gate — Rust ↔ TS ↔ JSON Schema ↔ Dart.

Exit 1 on ANY divergence. The Rust contract (crates/qrcode-ai-scanner/src/
report.rs + payload.rs + gs1.rs) is the SINGLE source of truth; the TS union
file, the JSON Schema and the hand-maintained Dart mirror must all agree — on
enum WIRE VALUES *and* on struct FIELD NAMES.

Surfaces compared:
  enums   : Symbology · EngineKind · EcLevel · Charset · Grade · UecGrade ·
            IsoGrade · StressAxis          (rust ↔ ts ↔ schema ↔ dart)
  payloads: payload.rs Payload             (rust ↔ ts kinds ↔ schema ↔ dart)
  hints   : report.rs  Hint                (rust ↔ ts tags  ↔ schema ↔ dart)
  structs : every contract struct's serde field names
            ↔ ts interface fields ↔ schema `required` ↔ dart fromJson wire keys

Wire-name policy the gate relies on:
  * Contract enums carry `#[serde(rename_all = "snake_case")]`; a variant's
    wire value is `snake(Variant)`.
  * Contract structs carry NO struct-level `rename_all`; fields are authored
    snake_case, so the serde/wire name == the Rust field name (per-field
    `#[serde(rename)]`/`skip` IS honoured if it ever appears).
  * The Dart mirror lenient-degrades UNKNOWN variants at runtime (every enum
    has an `unknown` fallback; Payload/Hint have catch-all variants). The gate
    checks only that its KNOWN set matches Rust EXACTLY. Dart field NAMES are
    read from the `j['wire_key']` accessors in each `fromJson` (the camelCase
    Dart identifiers are local and never touch the wire).

Schema leniency is deliberate (additive evolution): the tag property stays
`type: string`, but every FIELDED variant must have a conditional and the
fieldless set must be a subset of the documented union.
"""

import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

REPORT_RS = "crates/qrcode-ai-scanner/src/report.rs"
PAYLOAD_RS = "crates/qrcode-ai-scanner/src/payload.rs"
GS1_RS = "crates/qrcode-ai-scanner/src/gs1.rs"
TS_PATH = "bindings/report-types.d.ts"
DART_PATH = "bindings/flutter/lib/report.dart"

# Contract structs: (rust source, name). The mirror name is identical on every
# surface (ts interface · schema $def · dart class). ScanReport is the schema
# root (its `required` is top-level, not under $defs).
CONTRACT_STRUCTS = [
    (REPORT_RS, "Point"),
    (GS1_RS, "Gs1Element"),
    (REPORT_RS, "DecodedContent"),
    (REPORT_RS, "QrMeta"),
    (REPORT_RS, "Detection"),
    (REPORT_RS, "AxisScore"),
    (REPORT_RS, "StructuralReport"),
    (REPORT_RS, "UecReport"),
    (REPORT_RS, "IsoParameter"),
    (REPORT_RS, "Iso15415Report"),
    (REPORT_RS, "Score"),
    (REPORT_RS, "AlphaProbe"),
    (REPORT_RS, "AlphaEnvelope"),
    (REPORT_RS, "AlphaReport"),
    (REPORT_RS, "StageTrace"),
    (REPORT_RS, "PipelineTrace"),
    (REPORT_RS, "Versions"),
    (REPORT_RS, "ScanReport"),
]

# Fields the wire OMITS entirely when None (serde skip_serializing_if):
# present in the Rust struct + TS (`?:`) + Dart reads, ABSENT from the
# schema's `required` (but its `properties` must still describe them).
OPTIONAL_FIELDS: dict[str, set[str]] = {"ScanReport": {"alpha"}}

# Flat enums with a same-named schema $def (rust ↔ ts ↔ schema).
FLAT_ENUMS = [
    "Symbology",
    "EngineKind",
    "EcLevel",
    "Charset",
    "Grade",
    "IsoGrade",
    "StressAxis",
    "AlphaMode",
    "AlphaPlacement",
]

# Dart enums whose wire values mirror a same-named Rust enum. UecGrade/IsoGrade
# are handled apart: Dart folds them into one `LetterGrade` (see main()).
DART_ENUMS = [
    "Symbology",
    "EngineKind",
    "EcLevel",
    "Charset",
    "Grade",
    "StressAxis",
    "AlphaMode",
    "AlphaPlacement",
]


def snake(name: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def _read(path: str) -> str:
    return (ROOT / path).read_text()


def _balanced(src: str, open_idx: int, open_ch: str = "{", close_ch: str = "}") -> str:
    """Substring from the opening bracket at `open_idx` through its match, inclusive."""
    if src[open_idx] != open_ch:
        raise ValueError(f"expected {open_ch!r} at {open_idx}, found {src[open_idx]!r}")
    depth = 0
    for i in range(open_idx, len(src)):
        if src[i] == open_ch:
            depth += 1
        elif src[i] == close_ch:
            depth -= 1
            if depth == 0:
                return src[open_idx : i + 1]
    raise ValueError("unbalanced bracket")


def _strip_ts_comments(src: str) -> str:
    src = re.sub(r"/\*.*?\*/", "", src, flags=re.S)  # block + JSDoc
    return re.sub(r"//[^\n]*", "", src)  # line


def fail(label: str, diff: set[str]) -> None:
    print(f"DRIFT [{label}]: {sorted(diff)}", file=sys.stderr)
    sys.exit(1)


def eq(label: str, a: set[str], b: set[str]) -> None:
    if a != b:
        fail(label, a ^ b)


# ─────────────────────────── Rust truth ───────────────────────────


def rust_variants(path: str, enum_name: str) -> set[str]:
    src = _read(path)
    start = src.index(f"pub enum {enum_name}")
    body_start = src.index("{", start)
    body = _balanced(src, body_start)
    # variant = identifier at indent-4 followed by `,` or `{` (skip nested fields)
    names = re.findall(r"^\s{4}([A-Z]\w*)\s*[,{]", body, re.M)
    return {snake(n) for n in names}


def rust_struct_fields(path: str, struct_name: str) -> set[str]:
    """Serde wire field names of a `pub struct` (honours per-field rename/skip)."""
    src = _read(path)
    m = re.search(rf"\bpub struct {struct_name}\b", src)
    if not m:
        fail(f"rust struct {struct_name} missing", {struct_name})
    body = _balanced(src, src.index("{", m.start()))
    fields: set[str] = set()
    rename: str | None = None
    skip = False
    for line in body.splitlines():
        s = line.strip()
        if s.startswith("#["):
            r = re.search(r'serde\([^)]*\brename\s*=\s*"([^"]+)"', s)
            if r:
                rename = r.group(1)
            if re.search(r"serde\([^)]*\bskip\b", s):
                skip = True
            continue
        fm = re.match(r"pub\s+(\w+)\s*:", s)
        if fm:
            if not skip:
                fields.add(rename or fm.group(1))
            rename, skip = None, False
    return fields


# ─────────────────────────── TS surface ───────────────────────────


def ts_tags(pattern: str) -> set[str]:
    return set(re.findall(pattern, _read(TS_PATH)))


def ts_union(type_name: str) -> set[str]:
    """String-literal members of `export type <name> = "a" | "b" | …;`."""
    m = re.search(rf"\b{type_name} =\s*([^;]+);", _read(TS_PATH))
    if not m:
        fail(f"ts type {type_name} missing", {type_name})
    return set(re.findall(r'"(\w+)"', m.group(1)))


def ts_interface_fields(name: str) -> set[str]:
    src = _strip_ts_comments(_read(TS_PATH))
    m = re.search(rf"\binterface {name}\b", src)
    if not m:
        fail(f"ts interface {name} missing", {name})
    inner = _balanced(src, src.index("{", m.start()))[1:-1]
    return set(re.findall(r"(\w+)\s*\??\s*:", inner))


# ─────────────────────────── Dart surface ───────────────────────────


def dart_enum_wire(enum_name: str) -> set[str]:
    """KNOWN wire values from a Dart enum's `from(Object? v) => switch (v) {…}`."""
    src = _read(DART_PATH)
    start = src.index(f"static {enum_name} from(Object? v)")
    body = _balanced(src, src.index("{", start))
    return set(re.findall(r"'(\w+)'\s*=>", body))


def dart_union_tags(cls: str) -> set[str]:
    """KNOWN discriminators from a sealed `factory <cls>.fromJson` switch."""
    src = _read(DART_PATH)
    start = src.index(f"factory {cls}.fromJson")
    body = _balanced(src, src.index("{", start))  # params use (), so first { is the body
    return set(re.findall(r"'(\w+)'\s*=>", body))


def dart_fromjson_keys(cls: str) -> set[str]:
    """Wire keys read via `j['key']` in a class's `fromJson` (arrow or block)."""
    src = _read(DART_PATH)
    start = src.index(f"factory {cls}.fromJson")
    popen = src.index("(", start)
    pos = popen + len(_balanced(src, popen, "(", ")"))  # just past the param list `)`
    while src[pos] in " \t\r\n":
        pos += 1
    if src[pos] == "{":
        body = _balanced(src, pos)
    elif src[pos : pos + 2] == "=>":
        depth, i = 0, pos + 2
        body = None
        while i < len(src):
            c = src[i]
            if c in "([{":
                depth += 1
            elif c in ")]}":
                depth -= 1
            elif c == ";" and depth == 0:
                body = src[pos:i]
                break
            i += 1
        if body is None:
            raise ValueError(f"unterminated arrow body for {cls}.fromJson")
    else:
        raise ValueError(f"cannot parse {cls}.fromJson body")
    return set(re.findall(r"j\['(\w+)'\]", body))


# ─────────────────────────── the gate ───────────────────────────


def main() -> None:
    doc = json.loads(_read("spec/scan-report.schema.json"))

    # ---- payload kinds (rust ↔ ts ↔ schema ↔ dart) ----
    rust = rust_variants(PAYLOAD_RS, "Payload")
    eq("payload rust↔ts", rust, ts_tags(r'kind: "(\w+)"'))
    conds: set[str] = set()
    for cond in doc["$defs"]["Payload"]["allOf"]:
        kind = cond["if"]["properties"]["kind"]
        conds.update([kind["const"]] if "const" in kind else kind["enum"])
    fieldless = {"text"}  # the only kind with no fields at all
    if rust - conds - fieldless:
        fail("payload kinds missing a schema conditional", rust - conds - fieldless)
    if conds - rust:
        fail("schema conditionals for unknown kinds", conds - rust)
    eq("payload rust↔dart", rust, dart_union_tags("Payload"))

    # ---- hints (rust ↔ ts ↔ schema ↔ dart) ----
    rust_h = rust_variants(REPORT_RS, "Hint")
    eq("hint rust↔ts", rust_h, ts_tags(r'hint: "(\w+)"'))
    hint_conds: set[str] = set()
    for cond in doc["$defs"]["Hint"]["allOf"]:
        tag = cond["if"]["properties"]["hint"]
        hint_conds.update([tag["const"]] if "const" in tag else tag["enum"])
    fielded = {
        "raise_error_correction",
        "fix_finder_pattern",
        "low_correction_margin",
        "alpha_background_dependent",
    }
    if fielded - hint_conds:
        fail("fielded hints missing a schema conditional", fielded - hint_conds)
    eq("hint rust↔dart", rust_h, dart_union_tags("Hint"))

    # ---- flat enums with a same-named schema def (rust ↔ ts ↔ schema) ----
    for enum_name in FLAT_ENUMS:
        rust_e = rust_variants(REPORT_RS, enum_name)
        eq(f"{enum_name} rust↔ts", rust_e, ts_union(enum_name))
        eq(f"{enum_name} rust↔schema", rust_e, set(doc["$defs"][enum_name]["enum"]))

    # ---- UecGrade (rust ↔ ts). The schema folds UecReport.grade /
    #      IsoParameter.grade into the IsoGrade def, so the two Rust enums MUST
    #      share one wire alphabet for that fold to hold. ----
    uec = rust_variants(REPORT_RS, "UecGrade")
    eq("UecGrade rust↔ts", uec, ts_union("UecGrade"))
    eq("UecGrade≡IsoGrade wire fold", uec, rust_variants(REPORT_RS, "IsoGrade"))

    # ---- struct fields (rust ↔ ts ↔ schema) ----
    for path, name in CONTRACT_STRUCTS:
        rf = rust_struct_fields(path, name)
        eq(f"{name} fields rust↔ts", rf, ts_interface_fields(name))
        required = set(doc["required"] if name == "ScanReport" else doc["$defs"][name]["required"])
        optional = OPTIONAL_FIELDS.get(name, set())
        props = doc["properties"] if name == "ScanReport" else doc["$defs"][name]["properties"]
        if optional - set(props):
            fail(f"{name} optional fields missing from schema properties", optional - set(props))
        eq(f"{name} fields rust↔schema", rf - optional, required)

    # ---- Dart mirror: enum wire values (KNOWN set == Rust exactly) ----
    for enum_name in DART_ENUMS:
        eq(f"{enum_name} rust↔dart", rust_variants(REPORT_RS, enum_name), dart_enum_wire(enum_name))
    letter = dart_enum_wire("LetterGrade")  # Dart folds UecGrade + IsoGrade
    eq("LetterGrade↔UecGrade rust↔dart", rust_variants(REPORT_RS, "UecGrade"), letter)
    eq("LetterGrade↔IsoGrade rust↔dart", rust_variants(REPORT_RS, "IsoGrade"), letter)

    # ---- Dart mirror: struct wire keys (rust ↔ dart fromJson) ----
    for path, name in CONTRACT_STRUCTS:
        eq(f"{name} fields rust↔dart", rust_struct_fields(path, name), dart_fromjson_keys(name))

    print(
        f"type parity OK: {len(FLAT_ENUMS) + 1} enums · payloads · hints · "
        f"{len(CONTRACT_STRUCTS)} structs — across rust · ts · schema · dart"
    )


if __name__ == "__main__":
    main()
