"""Type stubs for qrcode-ai-scanner (PyO3 native bindings).

QR decoding + scannability scoring for artistic, AI-generated and photo-captured
QR codes. Every call returns the same versioned ScanReport (the cross-surface
contract — full schema: spec/scan-report.schema.json).
"""
from typing import Any, TypedDict

__version__: str

class ScanReport(TypedDict):
    """The versioned scan result. Full contract: spec/scan-report.schema.json."""

    detections: list[dict[str, Any]]
    score: dict[str, Any]
    hints: list[Any]
    trace: dict[str, Any]
    versions: dict[str, Any]

def scan(image: bytes, profile: str = "full") -> ScanReport:
    """Decode + score an encoded image (PNG, JPEG, WebP, GIF).

    profile: ``"full"`` (quality gate) | ``"fast"`` (upload) | ``"frame"`` (no scoring).
    "No QR found" returns a report with empty ``detections``; raises ``ValueError``
    on invalid input or an unknown profile.
    """
    ...

def scan_frame(
    rgba: bytes, width: int, height: int, profile: str = "frame"
) -> ScanReport:
    """Decode + score a raw RGBA frame (e.g. a camera frame) — no image-format roundtrip.

    ``rgba`` must be ``width * height * 4`` bytes. Raises ``ValueError`` on invalid input.
    """
    ...
