"""Type stubs for qrcode-ai-scanner (PyO3 native bindings).

QR decoding + scannability scoring for artistic, AI-generated and photo-captured
QR codes. Every call returns the same versioned ScanReport (the cross-surface
contract — full schema: spec/scan-report.schema.json).
"""
from typing import Any, TypedDict

# A stub is read by type checkers, never imported: NotRequired (typing 3.11+)
# comes from typing_extensions so that Python 3.10 targets read it too.
from typing_extensions import NotRequired

__version__: str

class ScanReport(TypedDict):
    """The versioned scan result. Full contract: spec/scan-report.schema.json."""

    detections: list[dict[str, Any]]
    """Empty = no QR found — final only when ``trace["budget_exhausted"]`` is
    ``False``. Each detection's ``content``: ``raw`` (the payload bytes,
    base64), ``text`` and ``charset`` — ``"utf8"`` | ``"shift_jis"`` |
    ``"latin1"`` | ``"gb18030"`` | ``"eci"`` | ``"unknown"``. ``"unknown"``, the
    only lossy value, means there is no verified reading: use ``raw``; treat
    any other value, one you do not know included, as a complete reading of
    ``raw``. The optional ``eci`` key lists the ECI designators
    (``[{"designator": int, "start": int}]``; ``[]`` when signalled but not
    located) and ``fnc1`` is ``"first"`` or ``"second"`` when present. Outside
    the QR family ``raw`` is the decoding engine's reading, which under an ECI
    can omit a segment the engine could not read. Two detections denote the
    same content by ``symbology``, ``raw``, ``fnc1`` and ``eci``, never by
    ``text`` (spec/01 § Content)."""
    score: dict[str, Any] | None
    """The judgment of ``detections[0]``. ``None`` when no judgment was
    published: no detection · a configuration that computes no score
    (``profile="frame"``, every stress axis skipped) · a judgment the wall-clock
    budget interrupted (``trace["budget_exhausted"]`` is ``True``: not judged —
    retry with a larger budget or ``budget_ms=0``) · a contained internal fault.
    Never partial; never read ``None`` as ``0``."""
    hints: list[Any]
    alpha: NotRequired[dict[str, Any]]
    """Alpha-transparency handling — the key is ABSENT for opaque inputs and
    under ``alpha_background="none"`` (spec/01 § alpha)."""
    trace: dict[str, Any]
    """Why the scan succeeded or came back empty. ``trace["budget_exhausted"]``:
    ``True`` when the budget prevented planned work (the report is partial),
    ``False`` when every planned unit ran (final, equal to the unbounded
    report); absent on reports older than score_contract 5 (unknown, never
    ``False``)."""
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
    zero-millisecond budget). Unset keeps the profile default (full 4000 · fast 800
    · frame 80 ms). With a budget, where the run cuts is machine-dependent and a
    cut report says so (``trace["budget_exhausted"]``) — strictly reproducible
    reports need ``budget_ms=0`` (spec/02).
    score_skip_axes: stress axes excluded from scoring, engine-side (wire names,
    e.g. ``["perspective", "rotation"]`` for generated previews with no capture
    geometry) — their cells never run, the composite renormalizes, ``score.axes``
    omits them; an unknown name raises (spec/04 § skipping axes).
    score_skip_checks: report sections withheld (``["uec", "iso15415",
    "alpha_envelope"]``) — the wire carries ``None``, the section-driven hints
    never fire; the UEC margin is still measured and caps the value, so the
    composite never moves; an unknown name raises (spec/04 § skipping checks).
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
