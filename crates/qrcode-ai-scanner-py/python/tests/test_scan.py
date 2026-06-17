"""Smoke tests for the qrcode-ai-scanner Python binding (PyO3).

Run after `maturin develop` (or against an installed wheel):  pytest
"""
from pathlib import Path

import pytest

import qrcode_ai_scanner as qr

REPO = Path(__file__).resolve().parents[4]
CLEAN = sorted((REPO / "fixtures" / "clean").glob("*.png"))


def test_module_surface():
    assert isinstance(qr.__version__, str) and qr.__version__
    assert callable(qr.scan)
    assert callable(qr.scan_frame)


@pytest.mark.skipif(not CLEAN, reason="no clean fixtures present")
def test_decodes_a_clean_qr():
    report = qr.scan(CLEAN[0].read_bytes(), "fast")
    # Same versioned ScanReport contract as every other surface (spec/).
    assert isinstance(report, dict)
    assert {"detections", "score", "hints"} <= report.keys()
    assert len(report["detections"]) >= 1
    assert isinstance(report["detections"][0]["content"]["text"], str)


def test_bad_profile_raises():
    with pytest.raises(ValueError):
        qr.scan(b"\x89PNG\r\n", "nonsense")


def test_invalid_image_raises():
    with pytest.raises(ValueError):
        qr.scan(b"definitely not an image", "fast")
