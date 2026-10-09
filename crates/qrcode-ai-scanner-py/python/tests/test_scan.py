"""Tests for the qrcode-ai-scanner Python binding (PyO3).

Run after `maturin develop` (or a wheel install):  pytest
Optional deps unlock more coverage: jsonschema (spec contract), Pillow (frames).

Every assertion is exact or relational, and none depends on the clock: a
judgment the wall-clock budget cuts is absent (``score`` is None), so every
scan that asserts a score, or needs the whole ladder to decode, runs unbounded
(``budget_ms=0``). Under a profile budget, machine load alone would flip it.
"""
import base64
import importlib.metadata
import io
import json
import re
from pathlib import Path

import pytest

import qrcode_ai_scanner as qr

REPO = Path(__file__).resolve().parents[4]
SCHEMA_PATH = REPO / "spec" / "scan-report.schema.json"
SCHEMA = json.loads(SCHEMA_PATH.read_text()) if SCHEMA_PATH.exists() else None
CLEAN = sorted((REPO / "fixtures" / "clean").glob("*.png"))
V5Q = REPO / "fixtures" / "clean" / "gen_v5_q.png"
V5Q_TEXT = "https://qrcode-ai.com/c/v5q"
AXES = ["resolution", "blur", "contrast", "perspective", "rotation", "lighting"]
# The wire symbology each symbology fixture decodes as. The expected TEXT is
# corpus.toml's (the repo's ground truth), never restated here.
SYMBOLOGY_OF = {
    "aztec.png": "aztec",
    "code128.png": "code128",
    "code39.png": "code39",
    "datamatrix-gs1.png": "data_matrix",
    "datamatrix.png": "data_matrix",
    "ean13.png": "ean13",
    "ean8.png": "ean8",
    "exif6-rotated-ean.jpg": "ean13",
    "itf14.png": "itf",
    "microqr.png": "micro_qr_code",
    "pdf417.png": "pdf417",
    "upca.png": "upc_a",
}

try:
    import tomllib

    CORPUS = {
        e["path"]: e.get("expected")
        for e in tomllib.loads((REPO / "corpus.toml").read_text())["entry"]
    }
except ModuleNotFoundError:  # Python < 3.11
    CORPUS = None

try:
    import jsonschema  # noqa: F401
    HAS_JSONSCHEMA = True
except ImportError:
    HAS_JSONSCHEMA = False

try:
    from PIL import Image
    HAS_PIL = True
except ImportError:
    HAS_PIL = False


def texts(report):
    return [d["content"]["text"] for d in report["detections"]]


def crate_version():
    manifest = (REPO / "crates" / "qrcode-ai-scanner-py" / "Cargo.toml").read_text()
    return re.search(r'^version = "([^"]+)"', manifest, re.MULTILINE).group(1)


def test_module_surface():
    # One version end to end: the crate manifest, the compiled module and the
    # installed distribution's metadata.
    assert qr.__version__ == crate_version()
    assert importlib.metadata.version("qrcode-ai-scanner") == qr.__version__
    assert callable(qr.scan) and callable(qr.scan_frame)


@pytest.mark.skipif(not SCHEMA, reason="no spec/ schema")
def test_report_versions_match_the_contract():
    markers = SCHEMA["$defs"]["Versions"]["properties"]
    report = qr.scan(V5Q.read_bytes(), "fast", budget_ms=0)
    assert report["versions"] == {
        "scanner": qr.__version__,
        "pipeline": markers["pipeline"]["const"],
        "score_contract": markers["score_contract"]["const"],
    }


def test_decodes_clean():
    report = qr.scan(V5Q.read_bytes(), "fast", budget_ms=0)
    assert texts(report) == [V5Q_TEXT]
    det = report["detections"][0]
    assert det["symbology"] == "qr_code"
    assert base64.b64decode(det["content"]["raw"]) == V5Q_TEXT.encode()
    assert det["content"]["charset"] == "utf8"
    assert (det["payload"]["kind"], det["payload"]["url"]) == ("url", V5Q_TEXT)
    meta = det["meta"]
    assert (meta["version"], meta["ec_level"], meta["modules"], meta["inverted"]) == (5, "q", 37, False)


def test_max_dimension_cap_rejects_oversized():
    # A 1px cap is below any real QR image → the size limit (QRS-002) raises ValueError.
    with pytest.raises(ValueError, match=r"image 360x360 exceeds limit 1 \[QRS-002\]"):
        qr.scan(V5Q.read_bytes(), max_dimension=1)


def test_generous_limits_still_decode():
    # Explicit large caps don't disturb a normal decode.
    report = qr.scan(V5Q.read_bytes(), "fast", max_dimension=20000, max_pixels=400_000_000, budget_ms=0)
    assert texts(report) == [V5Q_TEXT]


def test_budget_zero_is_unbounded():
    # 0 = unbounded (NOT a zero-millisecond budget) — the cross-binding convention
    # from spec/02. The full profile scores a pristine EC-Q symbol completely.
    report = qr.scan(V5Q.read_bytes(), "full", budget_ms=0)
    assert texts(report) == [V5Q_TEXT]
    score = report["score"]
    assert score["weights_run"] == 100
    assert [a["axis"] for a in score["axes"]] == AXES
    assert score["uec"]["grade"] == "a"
    assert isinstance(score["value"], int) and 70 <= score["value"] <= 100


def test_generous_budget_keeps_the_contract():
    # A 10-minute budget cannot cut a clean-fixture scan: the budget PLUMBING
    # (the Custom-profile path) returns the very judgment of the unbounded run.
    budgeted = qr.scan(V5Q.read_bytes(), "full", budget_ms=600_000)
    unbounded = qr.scan(V5Q.read_bytes(), "full", budget_ms=0)
    assert budgeted["detections"] == unbounded["detections"]
    assert budgeted["score"] == unbounded["score"]


@pytest.mark.skipif(not HAS_PIL, reason="needs Pillow")
def test_scan_frame_accepts_budget():
    im = Image.open(V5Q).convert("RGBA")
    report = qr.scan_frame(im.tobytes(), im.width, im.height, "frame", budget_ms=600_000)
    assert texts(report) == [V5Q_TEXT]
    assert report["score"] is None  # the frame profile never scores


@pytest.mark.skipif(
    not (CLEAN and HAS_JSONSCHEMA and SCHEMA), reason="needs fixtures + jsonschema + schema"
)
@pytest.mark.parametrize("img", CLEAN, ids=lambda p: p.name)
def test_output_conforms_to_spec_schema(img):
    # SOTA cross-surface contract: the Python dict validates against the SAME
    # spec/ JSON Schema as the Rust / Node / WASM surfaces.
    jsonschema.validate(qr.scan(img.read_bytes(), "full", budget_ms=0), SCHEMA)


def test_symbology_table_covers_every_fixture():
    on_disk = sorted(p.name for p in (REPO / "fixtures" / "symbology").iterdir() if p.is_file())
    assert on_disk == sorted(SYMBOLOGY_OF)


@pytest.mark.skipif(CORPUS is None, reason="corpus.toml needs tomllib (Python >= 3.11)")
@pytest.mark.parametrize("name", sorted(SYMBOLOGY_OF))
def test_symbologies_decode(name):
    report = qr.scan((REPO / "fixtures" / "symbology" / name).read_bytes(), "full", budget_ms=0)
    expected = CORPUS[f"fixtures/symbology/{name}"]
    assert [(d["symbology"], d["content"]["text"]) for d in report["detections"]] == [
        (SYMBOLOGY_OF[name], expected)
    ]


@pytest.mark.skipif(not HAS_PIL, reason="needs Pillow")
def test_scan_frame_rgba_matches_encoded():
    im = Image.open(V5Q).convert("RGBA")
    via_frame = qr.scan_frame(im.tobytes(), im.width, im.height, "frame", budget_ms=0)
    via_bytes = qr.scan(V5Q.read_bytes(), "frame", budget_ms=0)
    assert texts(via_frame) == texts(via_bytes) == [V5Q_TEXT]


@pytest.mark.skipif(not HAS_PIL, reason="needs Pillow")
def test_no_qr_is_ok_empty_not_error():
    buf = io.BytesIO()
    Image.new("RGB", (64, 64), "white").save(buf, format="PNG")
    report = qr.scan(buf.getvalue(), "fast", budget_ms=0)
    assert report["detections"] == []  # valid input, no QR → Ok, not an exception
    assert report["score"] is None  # nothing decoded, nothing judged


def test_bad_profile_raises():
    with pytest.raises(ValueError, match=r'unknown profile "nonsense"'):
        qr.scan(b"\x89PNG\r\n", "nonsense")


def test_invalid_image_raises():
    # The QRS-xxx wire code must ride in the message (parity with node/wasm/uniffi/
    # flutter — ScanError's Display omits it, so the binding appends `[QRS-xxx]`).
    with pytest.raises(ValueError, match=r"\[QRS-001\]"):
        qr.scan(b"definitely not an image", "fast")


def test_scan_frame_wrong_buffer_size_raises():
    # rgba must be width * height * 4 bytes — a mismatch is the most likely caller mistake.
    with pytest.raises(ValueError, match=r"buffer length 10 does not match expected 16 \[QRS-004\]"):
        qr.scan_frame(b"\x00" * 10, 2, 2, "frame")  # expects 2*2*4 = 16


def test_scan_frame_zero_dimension_raises():
    with pytest.raises(ValueError, match=r"zero dimension: 0x1 \[QRS-001\]"):
        qr.scan_frame(b"", 0, 1, "frame")


def test_all_exported():
    assert set(qr.__all__) == {"scan", "scan_frame", "__version__"}


def test_score_skip_axes_thread_through_and_reject_typos():
    # Skipped axes are absent from the wire (the engine never ran them and
    # renormalized the composite); a typo'd name raises loudly — never a
    # silent six-axis score.
    report = qr.scan(
        V5Q.read_bytes(),
        "full",
        budget_ms=0,
        score_skip_axes=["perspective", "rotation"],
    )
    assert [a["axis"] for a in report["score"]["axes"]] == ["resolution", "blur", "contrast", "lighting"]
    assert report["score"]["weights_run"] == 70

    with pytest.raises(ValueError, match="unknown stress axis"):
        qr.scan(V5Q.read_bytes(), "full", score_skip_axes=["perspektive"])


def test_score_skip_checks_null_sections_and_reject_typos():
    # The skipped sections are None and nothing else moves: a skipped check
    # never changes the composite (spec/04).
    report = qr.scan(
        V5Q.read_bytes(),
        "full",
        budget_ms=0,
        score_skip_checks=["uec", "iso15415"],
    )
    unskipped = qr.scan(V5Q.read_bytes(), "full", budget_ms=0)
    assert report["score"]["uec"] is None
    assert report["score"]["iso15415"] is None
    assert report["score"]["value"] == unskipped["score"]["value"]
    assert report["score"]["axes"] == unskipped["score"]["axes"]
    with pytest.raises(ValueError, match="unknown score check"):
        qr.scan(V5Q.read_bytes(), "full", score_skip_checks=["margin"])
