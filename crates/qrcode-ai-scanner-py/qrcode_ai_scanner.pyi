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

def scan(
    image: bytes,
    profile: str = "full",
    max_dimension: int | None = None,
    max_pixels: int | None = None,
    budget_ms: int | None = None,
) -> ScanReport:
    """Decode + score an encoded image (PNG, JPEG, WebP, GIF).

    profile: ``"full"`` (quality gate) | ``"fast"`` (upload) | ``"frame"`` (no scoring).
    max_dimension / max_pixels: optional input-size caps (defaults 10000 px / 64M px) —
    raise for huge images, lower to harden a server against adversarial input.
    budget_ms: overrides the profile's wall-clock budget; ``0`` = unbounded (NOT a
    zero-millisecond budget). With a budget set, where the run cuts is
    machine-dependent — leave unset/0 for strictly reproducible reports (spec/02).
    "No QR found" returns a report with empty ``detections``; raises ``ValueError``
    on invalid input, an oversized image, or an unknown profile.
    """
    ...

def scan_frame(
    rgba: bytes,
    width: int,
    height: int,
    profile: str = "frame",
    max_dimension: int | None = None,
    max_pixels: int | None = None,
    budget_ms: int | None = None,
) -> ScanReport:
    """Decode + score a raw RGBA frame (e.g. a camera frame) — no image-format roundtrip.

    ``rgba`` must be ``width * height * 4`` bytes. max_dimension / max_pixels: optional
    input-size caps (see ``scan``). budget_ms: per-frame wall-clock bound
    (``0`` = unbounded, see ``scan``). Raises ``ValueError`` on invalid input.
    """
    ...
