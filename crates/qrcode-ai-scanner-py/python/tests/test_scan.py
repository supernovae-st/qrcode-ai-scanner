"""Tests for the qrcode-ai-scanner Python binding (PyO3).

Run after `maturin develop`:  pytest
Optional deps unlock more coverage: jsonschema (spec contract), Pillow (frames).
"""
import io
import json
from pathlib import Path

import pytest

import qrcode_ai_scanner as qr

REPO = Path(__file__).resolve().parents[4]
SCHEMA_PATH = REPO / "spec" / "scan-report.schema.json"
SCHEMA = json.loads(SCHEMA_PATH.read_text()) if SCHEMA_PATH.exists() else None
CLEAN = sorted((REPO / "fixtures" / "clean").glob("*.png"))
SYMBOLOGY = sorted((REPO / "fixtures" / "symbology").glob("*.png"))

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


def test_module_surface():
    assert isinstance(qr.__version__, str) and qr.__version__
    assert callable(qr.scan) and callable(qr.scan_frame)


@pytest.mark.skipif(not CLEAN, reason="no clean fixtures")
def test_decodes_clean():
    report = qr.scan(CLEAN[0].read_bytes(), "fast")
    assert isinstance(report, dict)
    assert {"detections", "score", "hints"} <= report.keys()
    assert len(report["detections"]) >= 1
    assert isinstance(report["detections"][0]["content"]["text"], str)


@pytest.mark.skipif(not CLEAN, reason="no clean fixtures")
def test_max_dimension_cap_rejects_oversized():
    # A 1px cap is below any real QR image → the size limit (QRS-002) raises ValueError.
    with pytest.raises(ValueError):
        qr.scan(CLEAN[0].read_bytes(), max_dimension=1)


@pytest.mark.skipif(not CLEAN, reason="no clean fixtures")
def test_generous_limits_still_decode():
    # Explicit large caps don't disturb a normal decode.
    report = qr.scan(CLEAN[0].read_bytes(), "fast", max_dimension=20000, max_pixels=400_000_000)
    assert len(report["detections"]) >= 1


@pytest.mark.skipif(not CLEAN, reason="no clean fixtures")
def test_budget_zero_is_unbounded():
    # 0 = unbounded (NOT a zero-millisecond budget) — the cross-binding convention
    # from spec/02. Deterministic: no wall-clock dependence.
    report = qr.scan(CLEAN[0].read_bytes(), "full", budget_ms=0)
    assert len(report["detections"]) >= 1
    assert report["score"]  # full profile still scores unbounded


@pytest.mark.skipif(not CLEAN, reason="no clean fixtures")
def test_generous_budget_keeps_the_contract():
    # A 10-minute budget cannot cut a clean-fixture scan: asserts the budget
    # PLUMBING (the Custom-profile path) without wall-clock flakiness.
    report = qr.scan(CLEAN[0].read_bytes(), "full", budget_ms=600_000)
    assert len(report["detections"]) >= 1
    assert report["score"]


@pytest.mark.skipif(not (CLEAN and HAS_PIL), reason="needs fixtures + Pillow")
def test_scan_frame_accepts_budget():
    im = Image.open(CLEAN[0]).convert("RGBA")
    report = qr.scan_frame(im.tobytes(), im.width, im.height, "frame", budget_ms=600_000)
    assert len(report["detections"]) >= 1


@pytest.mark.skipif(
    not (CLEAN and HAS_JSONSCHEMA and SCHEMA), reason="needs fixtures + jsonschema + schema"
)
@pytest.mark.parametrize("img", CLEAN, ids=lambda p: p.name)
def test_output_conforms_to_spec_schema(img):
    # SOTA cross-surface contract: the Python dict validates against the SAME
    # spec/ JSON Schema as the Rust / Node / WASM surfaces.
    jsonschema.validate(qr.scan(img.read_bytes(), "full"), SCHEMA)


@pytest.mark.skipif(not SYMBOLOGY, reason="no symbology fixtures")
@pytest.mark.parametrize("img", SYMBOLOGY, ids=lambda p: p.name)
def test_symbologies_decode(img):
    report = qr.scan(img.read_bytes(), "full")
    assert len(report["detections"]) >= 1
    assert report["detections"][0]["symbology"]


@pytest.mark.skipif(not (CLEAN and HAS_PIL), reason="needs fixtures + Pillow")
def test_scan_frame_rgba_matches_encoded():
    im = Image.open(CLEAN[0]).convert("RGBA")
    via_frame = qr.scan_frame(im.tobytes(), im.width, im.height, "frame")
    via_bytes = qr.scan(CLEAN[0].read_bytes(), "frame")
    assert via_frame["detections"][0]["content"]["text"] == (
        via_bytes["detections"][0]["content"]["text"]
    )


@pytest.mark.skipif(not HAS_PIL, reason="needs Pillow")
def test_no_qr_is_ok_empty_not_error():
    buf = io.BytesIO()
    Image.new("RGB", (64, 64), "white").save(buf, format="PNG")
    report = qr.scan(buf.getvalue(), "fast")
    assert report["detections"] == []  # valid input, no QR → Ok, not an exception


def test_bad_profile_raises():
    with pytest.raises(ValueError):
        qr.scan(b"\x89PNG\r\n", "nonsense")


def test_invalid_image_raises():
    # The QRS-xxx wire code must ride in the message (parity with node/wasm/uniffi/
    # flutter — ScanError's Display omits it, so the binding appends `[QRS-xxx]`).
    with pytest.raises(ValueError, match=r"\[QRS-001\]"):
        qr.scan(b"definitely not an image", "fast")


def test_scan_frame_wrong_buffer_size_raises():
    # rgba must be width * height * 4 bytes — a mismatch is the most likely caller mistake.
    with pytest.raises(ValueError, match=r"\[QRS-004\]"):
        qr.scan_frame(b"\x00" * 10, 2, 2, "frame")  # expects 2*2*4 = 16


def test_scan_frame_zero_dimension_raises():
    with pytest.raises(ValueError):
        qr.scan_frame(b"", 0, 1, "frame")


def test_all_exported():
    assert set(qr.__all__) == {"scan", "scan_frame", "__version__"}


def test_score_skip_axes_thread_through_and_reject_typos():
    # Skipped axes are absent from the wire (the engine never ran them and
    # renormalized the composite); a typo'd name raises loudly — never a
    # silent six-axis score.
    report = qr.scan(
        CLEAN[0].read_bytes(),
        "full",
        budget_ms=0,
        score_skip_axes=["perspective", "rotation"],
    )
    axes = report["score"]["axes"]
    assert len(axes) == 4, axes
    assert all(a["axis"] not in ("perspective", "rotation") for a in axes)

    with pytest.raises(ValueError, match="unknown stress axis"):
        qr.scan(CLEAN[0].read_bytes(), "full", score_skip_axes=["perspektive"])


def test_score_skip_checks_null_sections_and_reject_typos():
    report = qr.scan(
        CLEAN[0].read_bytes(),
        "full",
        score_skip_checks=["uec", "iso15415"],
    )
    assert report["score"]["uec"] is None
    assert report["score"]["iso15415"] is None
    assert isinstance(report["score"]["value"], int)
    with pytest.raises(ValueError, match="unknown score check"):
        qr.scan(CLEAN[0].read_bytes(), "full", score_skip_checks=["margin"])

