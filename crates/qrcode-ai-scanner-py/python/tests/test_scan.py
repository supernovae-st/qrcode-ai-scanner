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
    with pytest.raises(ValueError):
        qr.scan(b"definitely not an image", "fast")
