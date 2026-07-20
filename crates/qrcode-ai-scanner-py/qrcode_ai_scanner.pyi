"""Type stubs for qrcode-ai-scanner (PyO3 native bindings).

QR decoding + scannability scoring for artistic, AI-generated and photo-captured
QR codes. Every call returns the same versioned ScanReport (the cross-surface
contract — full schema: spec/scan-report.schema.json).
"""
from typing import Any, NotRequired, TypedDict

__version__: str

class ScanReport(TypedDict):
    """The versioned scan result. Full contract: spec/scan-report.schema.json."""

    detections: list[dict[str, Any]]
    score: dict[str, Any]
    hints: list[Any]
    alpha: NotRequired[dict[str, Any]]
    """Alpha-transparency handling — the key is ABSENT for opaque inputs and
    under ``alpha_background="none"`` (spec/01 § alpha)."""
    trace: dict[str, Any]
    versions: dict[str, Any]

def scan(
    image: bytes,
    profile: str = "full",
    max_dimension: int | None = None,
    max_pixels: int | None = None,
    budget_ms: int | None = None,
    score_skip_axes: list[str] | None = None,
    score_skip_checks: list[str] | None = None,
    alpha_background: str | None = None,
    alpha_palette: list[str] | None = None,
    score_preset: str | None = None,
) -> ScanReport:
    """Decode + score an encoded image (PNG, JPEG, WebP, GIF).

    profile: ``"full"`` (quality gate) | ``"fast"`` (upload) | ``"frame"`` (no scoring).
    max_dimension / max_pixels: optional input-size caps (defaults 10000 px / 64M px) —
    raise for huge images, lower to harden a server against adversarial input.
    budget_ms: overrides the profile's wall-clock budget; ``0`` = unbounded (NOT a
    zero-millisecond budget). With a budget set, where the run cuts is
    machine-dependent — leave unset/0 for strictly reproducible reports (spec/02).
    score_skip_axes: stress axes excluded from scoring, engine-side (wire names,
    e.g. ``["perspective", "rotation"]`` for generated previews with no capture
    geometry) — their cells never run, the composite renormalizes, ``score.axes``
    omits them; an unknown name raises (spec/04 § skipping axes).
    score_skip_checks: report sections excluded at the source (``["uec", "iso15415",
    "alpha_envelope"]``) — never computed, the wire carries ``None``, the
    section-driven hints never fire; the composite value does not move; an unknown
    name raises (spec/04 § skipping checks).
    alpha_palette: theme/brand colors (``["white", "black", "#rrggbb", ...]``)
    probed by the placement envelope inside the same scan — per-color verdicts in
    ``report["alpha"]["envelope"]["palette"]`` (request order); an unknown value raises.
    score_preset: named axis posture — ``"design"`` (generated previews: skips
    perspective+rotation, keeps lighting) | ``"capture"`` (all six axes). Mutually
    exclusive with score_skip_axes (raises). The report carries ``score.weights_run``
    (Σ weights of the axes that ran) so a partial score says how much contract
    stands behind it.
    alpha_background: the background flattened under transparent pixels —
    ``"auto"`` (default: the design's own content picks it, with an opposite-
    background retry on zero detections) | ``"white"`` | ``"black"`` |
    ``"#rrggbb"`` (the host's real placement surface) | ``"none"`` (pre-0.9
    drop-the-channel behavior). Opaque inputs are untouched; transparent inputs
    report the resolved background + placement envelope in ``report["alpha"]``.
    An unknown value raises (spec/01 § alpha).
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
    score_skip_axes: list[str] | None = None,
    score_skip_checks: list[str] | None = None,
    alpha_background: str | None = None,
    alpha_palette: list[str] | None = None,
    score_preset: str | None = None,
) -> ScanReport:
    """Decode + score a raw RGBA frame (e.g. a camera frame) — no image-format roundtrip.

    ``rgba`` must be ``width * height * 4`` bytes. max_dimension / max_pixels: optional
    input-size caps (see ``scan``). budget_ms: per-frame wall-clock bound
    (``0`` = unbounded, see ``scan``). score_skip_axes / score_skip_checks /
    alpha_background: see ``scan``.
    Raises ``ValueError`` on invalid input.
    """
    ...
