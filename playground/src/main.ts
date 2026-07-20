import init, { scan_image, scan_frame, version } from '@supernovae-st/qrcode-ai-scanner-wasm';
import type {
  ScanReport, Score, AxisScore, Detection, Payload, Hint,
  Iso15415Report, IsoParameter, UecReport, Grade, AlphaReport, AlphaEnvelope,
} from '@supernovae-st/qrcode-ai-scanner-wasm';
import './style.css';

/* ---------- DOM ---------- */
const $ = <T extends Element>(s: string): T => document.querySelector(s) as T;
const statusEl = $('#status'), statusText = $('#status-text'), versionEl = $('#version');
const dropzone = $('#dropzone'), fileInput = $<HTMLInputElement>('#file');
const readout = $('#readout');
const previewFig = $<HTMLElement>('#preview');
const previewImg = $<HTMLImageElement>('#preview-img');
const previewCap = $('#preview-cap');
const cornersSvg = $<SVGSVGElement>('#corners');
const video = $<HTMLVideoElement>('#video');
const frameCanvas = $<HTMLCanvasElement>('#frame-canvas');
const cameraBtn = $<HTMLButtonElement>('#camera-btn');
const frameCtx = frameCanvas.getContext('2d', { willReadFrequently: true });

/* ---------- tiny safe hyperscript (text via text nodes — never innerHTML) ---------- */
type Kid = Node | string | null | undefined | false;
function h(tag: string, attrs: Record<string, unknown> = {}, ...kids: Kid[]): HTMLElement {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === undefined || v === false || v === null) continue;
    if (k === 'class') el.className = String(v);
    else el.setAttribute(k, String(v));
  }
  for (const kid of kids) {
    if (kid === null || kid === undefined || kid === false) continue;
    el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  }
  return el;
}

/* ---------- state ---------- */
let ready = false;
let lastBytes: Uint8Array | null = null;
let lastDetection: Detection | null = null;
let blobUrl: string | null = null;

/* ---------- helpers ---------- */
const profile = (): string => ($('input[name=profile]:checked') as HTMLInputElement)?.value ?? 'full';
const budget = (): number => Number(($('#budget') as HTMLInputElement).value) || 0;
// undefined = the engine default (auto) — the explicit value only when forced,
// so the default path stays byte-identical to a host that passes nothing.
const alphaMode = (): string | undefined => {
  const v = ($('input[name=alphamode]:checked') as HTMLInputElement)?.value ?? 'auto';
  return v === 'auto' ? undefined : v;
};
// The host-palette demo: two theme colors probed inside the same scan —
// their per-color verdicts land in alpha.envelope.palette (transparent
// inputs only; opaque scans ignore the whole alpha config).
const DEMO_PALETTE = ['#f5f5f4', '#1c1917'];

const GRADE_LETTER: Record<Grade, string> = {
  excellent: 'a', good: 'b', acceptable: 'c', fair: 'd', poor: 'f',
};
const lvl = (letter: string) => `lvl-${letter.toLowerCase()}`;
const cap = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

// Render C0/C1 control bytes (e.g. the GS1 FNC1 separator 0x1D) as visible
// Unicode "control pictures" — honest display, never a raw control char.
const printable = (s: string) =>
  Array.from(s, (c) => {
    const code = c.charCodeAt(0);
    if (code <= 0x1f) return String.fromCharCode(0x2400 + code);
    if (code === 0x7f) return String.fromCharCode(0x2421);
    if (code >= 0x80 && code <= 0x9f) return String.fromCharCode(0xfffd);
    return c;
  }).join("");

const camIdle = (): Kid[] => [h('span', { class: 'cam-glyph', 'aria-hidden': 'true' }, '◉'), ' Live camera ', h('span', { class: 'cam-sub' }, '(scan_frame)')];
const camLive = (): Kid[] => [h('span', { class: 'cam-glyph', 'aria-hidden': 'true' }, '■'), ' Stop camera'];

/* ---------- boot ---------- */
async function boot() {
  try {
    await init();
    ready = true;
    versionEl.textContent = `v${version()}`;
    statusEl.setAttribute('data-state', 'ready');
    statusText.textContent = 'engine ready';
  } catch (err) {
    statusEl.setAttribute('data-state', 'error');
    statusText.textContent = 'engine failed to load';
    showError(String(err));
    console.error(err);
  }
}

/* ---------- scan an encoded image ---------- */
function scanBytes(bytes: Uint8Array, label: string) {
  if (!ready) return;
  lastBytes = bytes;
  lastDetection = null; // clear before scan so a throw can't leave stale corners

  if (blobUrl) URL.revokeObjectURL(blobUrl);
  blobUrl = URL.createObjectURL(new Blob([bytes]));
  video.hidden = true; previewImg.hidden = false;
  previewImg.onload = () => placeCorners(lastDetection);
  previewImg.src = blobUrl;
  previewFig.hidden = false;
  previewCap.textContent = `${label} · ${bytes.length.toLocaleString()} bytes · profile ${profile()}`;

  try {
    const t0 = performance.now();
    const report = scan_image(
      bytes, profile(), undefined, undefined, budget(), undefined, undefined, alphaMode(),
      DEMO_PALETTE,
    ) as ScanReport;
    const wall = performance.now() - t0;
    lastDetection = report.detections[0] ?? null;
    renderReport(report, wall);
    if (previewImg.complete) placeCorners(lastDetection);
  } catch (err) {
    showError(String(err));
  }
}

/* ---------- corners overlay ---------- */
function placeCorners(det: Detection | null) {
  cornersSvg.replaceChildren();
  if (!det?.corners || !previewImg.naturalWidth || previewImg.hidden) return;
  cornersSvg.style.left = `${previewImg.offsetLeft}px`;
  cornersSvg.style.top = `${previewImg.offsetTop}px`;
  cornersSvg.style.width = `${previewImg.clientWidth}px`;
  cornersSvg.style.height = `${previewImg.clientHeight}px`;
  cornersSvg.setAttribute('viewBox', `0 0 ${previewImg.naturalWidth} ${previewImg.naturalHeight}`);
  cornersSvg.setAttribute('preserveAspectRatio', 'none');
  const ns = 'http://www.w3.org/2000/svg';
  const poly = document.createElementNS(ns, 'polygon');
  poly.setAttribute('points', det.corners.map((p) => `${p.x},${p.y}`).join(' '));
  cornersSvg.appendChild(poly);
  const r = Math.max(previewImg.naturalWidth, previewImg.naturalHeight) / 90;
  for (const p of det.corners) {
    const c = document.createElementNS(ns, 'circle');
    c.setAttribute('cx', String(p.x)); c.setAttribute('cy', String(p.y)); c.setAttribute('r', String(r));
    cornersSvg.appendChild(c);
  }
}
window.addEventListener('resize', () => placeCorners(lastDetection));

/* ---------- render ---------- */
function renderReport(r: ScanReport, wall: number) {
  const report = h('div', { class: 'report' });
  const d = r.detections[0];

  // 1 — verdict
  const verdict = h('div', { class: 'verdict' });
  const top = h('div', { class: 'verdict-top' });
  if (d) {
    top.append(h('span', { class: 'badge sym' }, d.symbology));
    top.append(h('span', { class: 'badge kind' }, d.payload.kind));
    if (d.engines.includes('rescue')) top.append(h('span', { class: 'badge rescue' }, '⛑ rescue'));
    else top.append(h('span', { class: 'badge muted' }, d.engines.join('+')));
  } else {
    top.append(h('span', { class: 'badge muted' }, 'no detection'));
  }
  top.append(h('span', { class: 'badge muted' }, `${(r.trace.total_ms ?? wall).toFixed(0)} ms`));
  if (r.detections.length > 1) top.append(h('span', { class: 'badge muted' }, `+${r.detections.length - 1} more`));
  verdict.append(top);
  verdict.append(
    d
      ? h('div', { class: 'verdict-text' }, printable(d.content.text) || '(empty)')
      : h('div', { class: 'verdict-text none' }, 'NO QR / BARCODE FOUND — a valid outcome, not an error'),
  );
  report.append(verdict);

  if (r.score) report.append(scoreModule(r.score));
  if (r.alpha) report.append(alphaModule(r.alpha));
  if (r.score?.uec) report.append(uecModule(r.score.uec));
  if (r.score?.iso15415) report.append(isoModule(r.score.iso15415));
  if (r.score) report.append(hintsModule(r.hints));
  if (d) report.append(payloadModule(d.payload, d.content.charset));
  if (d && d.meta && (d.meta.version != null || d.meta.modules != null)) report.append(metaModule(d));
  report.append(traceModule(r));
  report.append(
    h('details', { class: 'raw' },
      h('summary', {}, 'raw ScanReport (JSON)'),
      h('pre', {}, JSON.stringify(r, null, 2)),
    ),
  );

  readout.replaceChildren(report);
}

function scoreModule(s: Score): HTMLElement {
  const letter = GRADE_LETTER[s.grade] ?? 'f';
  const head = h('div', { class: 'score-head' },
    h('span', { class: 'score-num' }, String(s.value)),
    h('span', { class: 'score-den' }, '/ 100'),
    h('span', { class: `grade-chip ${lvl(letter)}` }, h('span', { class: 'dot' }), `${cap(s.grade)} · ${letter.toUpperCase()}`),
  );
  const axes = h('div', { class: 'axes' }, ...s.axes.map(axisRow));
  return h('section', { class: 'module' }, h('h2', {}, 'Scannability score'), head, axes);
}

function axisRow(a: AxisScore): HTMLElement {
  const ratio = a.total ? a.passed / a.total : 0;
  const color = ratio >= 1 ? 'var(--g-a)' : ratio > 0 ? 'var(--g-c)' : 'var(--g-f)';
  const bar = h('div', { class: 'axis-bar', role: 'img', 'aria-label': `${a.passed} of ${a.total}` });
  for (let i = 0; i < a.total; i++) {
    const cell = h('span', { class: 'axis-cell' + (i < a.passed ? ' on' : '') });
    if (i < a.passed) cell.style.background = color;
    bar.append(cell);
  }
  return h('div', { class: 'axis' },
    h('span', { class: 'axis-name' }, a.axis),
    bar,
    h('span', { class: 'axis-val' }, `${a.passed}/${a.total}`),
  );
}

function uecModule(u: UecReport): HTMLElement {
  const danger = u.margin <= 0;
  const fill = h('div', { class: 'uec-fill' });
  fill.style.width = `${Math.max(0, Math.min(1, u.margin)) * 100}%`;
  const track = h('div', { class: 'uec-track' }, fill);
  const meta = h('div', { class: 'uec-meta' },
    h('span', {}, `margin ${u.margin.toFixed(2)}`),
    h('span', { class: lvl(u.grade) }, `grade ${u.grade.toUpperCase()}`),
    h('span', {}, `worst block ${u.worst_block_errors}/${u.worst_block_capacity} ec`),
  );
  const mod = h('section', { class: `module uec${danger ? ' danger' : ''}` },
    h('h2', {}, 'Unused error correction (margin)'),
    track, meta,
  );
  if (danger) mod.append(h('div', { class: 'uec-warn' }, '⚠ margin 0 — decode sat at the RS limit; treat content as unverified'));
  return mod;
}

function isoModule(iso: Iso15415Report): HTMLElement {
  const chip = (k: string, p: IsoParameter | null) =>
    p
      ? h('div', { class: 'iso-chip' },
          h('span', { class: 'k' }, k),
          h('span', { class: `v ${lvl(p.grade)}` }, `${p.value.toFixed(2)} · ${p.grade.toUpperCase()}`))
      : h('div', { class: 'iso-chip' }, h('span', { class: 'k' }, k), h('span', { class: 'v' }, '—'));
  const card = h('div', { class: 'iso' },
    chip('contrast', iso.symbol_contrast),
    chip('modulation', iso.modulation),
    chip('axial', iso.axial_nonuniformity),
    chip('fixed-pat', iso.fixed_pattern_damage),
    chip('uec', iso.unused_error_correction),
    h('div', { class: `iso-chip iso-overall ${lvl(iso.overall)}` },
      h('span', { class: 'k' }, 'overall'),
      h('span', { class: 'v' }, iso.overall.toUpperCase())),
  );
  return h('section', { class: 'module' }, h('h2', {}, 'ISO 15415 card · informed, not certified'), card);
}

/* The alpha block: the flatten verdict + the placement envelope — "over
   which backgrounds does this transparent design keep decoding". Every
   probe swatch IS its tested background color; bands are tested endpoints. */
function alphaModule(a: AlphaReport): HTMLElement {
  const verdict = h('div', { class: 'alpha-verdict' },
    h('span', { class: 'badge muted' }, `flattened over ${a.background}`),
    h('span', { class: 'badge muted' }, `mode ${a.mode}`),
    h('span', { class: 'badge muted' }, `transparency ${(a.coverage * 100).toFixed(0)}%`),
    a.fallback_used ? h('span', { class: 'badge rescue' }, '⛑ opposite-background rescue') : null,
  );
  const mod = h('section', { class: 'module alpha' },
    h('h2', {}, 'Alpha · placement envelope'), verdict);
  if (a.envelope) {
    const strip = h('div', { class: 'alpha-strip', role: 'img',
      'aria-label': `decode probes over neutral backgrounds, placement ${a.envelope.placement}` });
    for (const p of a.envelope.probes) {
      const sw = h('span', { class: `alpha-probe${p.decoded ? ' on' : ''}`, title: `luma ${p.background_luma}` },
        p.decoded ? '✓' : '✕');
      sw.style.background = `rgb(${p.background_luma},${p.background_luma},${p.background_luma})`;
      sw.style.color = p.background_luma < 128 ? '#e8e8e8' : '#111';
      strip.append(sw);
    }
    const bands = a.envelope.safe_luma.map(([lo, hi]) => `${lo}–${hi}`).join(' · ') || 'none';
    mod.append(strip, h('div', { class: 'alpha-meta' },
      h('span', { class: `alpha-placement pl-${a.envelope.placement}` }, a.envelope.placement),
      h('span', {}, `safe background luma ${bands}`),
    ));
    const carousel = placementCarousel(a.envelope);
    if (carousel) mod.append(carousel);
  } else {
    mod.append(h('p', { class: 'hints-empty' }, 'envelope not swept (full profile only / skipped)'));
  }
  return mod;
}

/* The placement carousel: the user's ACTUAL design composited over each
   probed background (canvas fill + drawImage — the PNG's alpha composites
   naturally), verdict-bordered. Neutral rungs first, then the host
   palette verdicts. Tiles decode their OWN copy of the scanned bytes —
   never previewImg (its load races the render, and a slow load would
   paint the PREVIOUS design onto this report's tiles). */
function placementCarousel(env: AlphaEnvelope): HTMLElement | null {
  if (!lastBytes) return null;
  const NEUTRAL = [0, 64, 128, 192, 255];
  const tiles = [
    ...env.probes
      .filter((p) => NEUTRAL.includes(p.background_luma))
      .map((p) => ({
        fill: `rgb(${p.background_luma},${p.background_luma},${p.background_luma})`,
        decoded: p.decoded,
        label: `luma ${p.background_luma}`,
      })),
    ...env.palette.map((p) => ({
      fill: p.background === 'white' ? '#ffffff' : p.background === 'black' ? '#000000' : p.background,
      decoded: p.decoded,
      label: p.background,
    })),
  ];
  const row = h('div', { class: 'alpha-carousel' });
  const canvases: { canvas: HTMLCanvasElement; fill: string }[] = [];
  for (const t of tiles) {
    const canvas = h('canvas', {
      class: `alpha-tile ${t.decoded ? 'ok' : 'ko'}`, width: '112', height: '112',
    }) as unknown as HTMLCanvasElement;
    const ctx = canvas.getContext('2d');
    if (ctx) { ctx.fillStyle = t.fill; ctx.fillRect(0, 0, 112, 112); }
    canvases.push({ canvas, fill: t.fill });
    row.append(h('figure', { class: 'alpha-tile-wrap' },
      canvas,
      h('figcaption', {}, `${t.decoded ? '✓' : '✕'} ${t.label}`),
    ));
  }
  // swatches show instantly; the design lands as soon as ITS bytes decode
  const img = new Image();
  const url = URL.createObjectURL(new Blob([lastBytes]));
  img.onload = () => {
    for (const { canvas } of canvases) {
      canvas.getContext('2d')?.drawImage(img, 8, 8, 96, 96);
    }
    URL.revokeObjectURL(url);
  };
  img.onerror = () => URL.revokeObjectURL(url);
  img.src = url;
  return row;
}

const HINT_INFO: Record<string, { glyph: string; act: (x: any) => string; crit?: boolean }> = {
  fix_finder_pattern:     { glyph: '◳', act: (x) => `Clear the art off corner ${x.corner} (0=TL · 1=TR · 2=BL).` },
  restore_quiet_zone:     { glyph: '▢', act: () => 'Add a clean ≥2-module margin around the symbol.' },
  increase_contrast:      { glyph: '◐', act: () => 'Darken the modules or lighten the background.' },
  enlarge_modules:        { glyph: '⤢', act: () => 'Render bigger, or use a lower QR version.' },
  reduce_art_texture:     { glyph: '░', act: () => 'Lighten the texture over the data zone.' },
  raise_error_correction: { glyph: '↑', act: (x) => `Regenerate at a higher EC level (current: ${String(x.current).toUpperCase()}).` },
  low_correction_margin:  { glyph: '⚠', crit: true, act: (x) => `Decode at the RS limit (${x.errors}/${x.capacity}) — possible miscorrection. Verify out-of-band / regenerate.` },
  alpha_background_dependent: { glyph: '▞', act: (x) => `Only survives ${String(x.placement).replace('_', ' ')} backgrounds — pin a background layer, or place it inside the safe luma bands.` },
  add_background_plate: { glyph: '▣', act: (x) => `Add a ${x.color} plate (rounded rectangle + quiet-zone margin) behind the symbol — robust everywhere, transparent look preserved around it.` },
};

function hintsModule(hints: Hint[]): HTMLElement {
  const mod = h('section', { class: 'module' }, h('h2', {}, `Hints · feedback loop (${hints.length})`));
  if (!hints.length) { mod.append(h('p', { class: 'hints-empty' }, '✓ clean — no improvement hints.')); return mod; }
  const list = h('div', { class: 'hints' });
  for (const hint of hints) {
    const info = HINT_INFO[hint.hint] ?? { glyph: '•', act: () => '' };
    list.append(h('div', { class: `hint${info.crit ? ' crit' : ''}` },
      h('span', { class: 'hint-glyph' }, info.glyph),
      h('div', { class: 'hint-body' },
        h('span', { class: 'hint-name' }, hint.hint),
        h('span', { class: 'hint-act' }, info.act(hint as any))),
    ));
  }
  mod.append(list);
  return mod;
}

function payloadModule(p: Payload, charset: string): HTMLElement {
  const dl = h('dl', { class: 'kv' });
  const row = (k: string, v: Kid) => { dl.append(h('dt', {}, k), v instanceof Node ? h('dd', {}, v) : h('dd', {}, String(v))); };
  const link = (url: string): Kid =>
    /^https?:\/\//i.test(url)
      ? h('a', { href: url, target: '_blank', rel: 'noopener noreferrer' }, url)
      : document.createTextNode(url);
  const ok = (b: boolean) => h('span', { class: b ? 'pill-ok' : 'pill-no' }, b ? '✓ conformant' : '✕ not conformant');

  row('kind', p.kind);
  switch (p.kind) {
    case 'url': row('url', link(p.url)); break;
    case 'wifi':
      row('ssid', p.ssid); row('security', p.security);
      if (p.password) row('password', p.password);
      row('hidden', String(p.hidden)); break;
    case 'email': row('to', p.to); if (p.subject) row('subject', p.subject); if (p.body) row('body', p.body); break;
    case 'sms': row('number', p.number); if (p.body) row('body', p.body); break;
    case 'tel': row('number', p.number); break;
    case 'geo': row('lat', p.lat); row('lon', p.lon); break;
    case 'me_card':
      if (p.name) row('name', p.name); if (p.tel) row('tel', p.tel);
      if (p.email) row('email', p.email); if (p.url) row('url', link(p.url)); break;
    case 'crypto': row('scheme', p.scheme); row('address', p.address); if (p.amount) row('amount', p.amount); break;
    case 'v_card': case 'v_event': row('raw', p.raw.slice(0, 200)); break;
    case 'gs1': case 'gs1_digital_link': {
      if (p.kind === 'gs1_digital_link') row('url', link(p.url));
      if (p.gtin) row('gtin', p.gtin);
      row('conformant', ok(p.conformant));
      if (p.elements.length) row('elements', h('div', {}, ...p.elements.map((e) => h('div', {}, `AI ${e.ai} = ${e.value}`))));
      if (p.issues.length) dl.append(h('dt', {}, 'issues'), h('dd', {}, h('ul', { class: 'issues' }, ...p.issues.map((i) => h('li', {}, i)))));
      break;
    }
  }
  row('charset', charset);
  return h('section', { class: 'module' }, h('h2', {}, 'Payload'), dl);
}

function metaModule(d: Detection): HTMLElement {
  const m = d.meta;
  const dl = h('dl', { class: 'kv' });
  const row = (k: string, v: unknown) => { if (v != null) { dl.append(h('dt', {}, k), h('dd', {}, String(v))); } };
  row('version', m.version);
  row('ec level', m.ec_level ? String(m.ec_level).toUpperCase() : null);
  row('mask', m.mask);
  row('modules', m.modules != null ? `${m.modules} × ${m.modules}` : null);
  row('inverted', m.inverted);
  return h('section', { class: 'module' }, h('h2', {}, 'Symbol metadata'), dl);
}

function traceModule(r: ScanReport): HTMLElement {
  const dl = h('dl', { class: 'kv' });
  const decoded = r.trace.stages.filter((s) => s.detections_found > 0).map((s) => s.stage).join(', ') || '—';
  dl.append(h('dt', {}, 'decoded at'), h('dd', {}, decoded));
  dl.append(h('dt', {}, 'stages'), h('dd', {}, r.trace.stages.map((s) => `${s.stage}(${s.ms.toFixed(0)}ms)`).join(' → ')));
  dl.append(h('dt', {}, 'panics'), h('dd', {}, String(r.trace.engine_panics)));
  dl.append(h('dt', {}, 'versions'), h('dd', {}, `scanner ${r.versions.scanner} · pipeline ${r.versions.pipeline} · score ${r.versions.score_contract}`));
  return h('section', { class: 'module' }, h('h2', {}, 'Pipeline trace'), dl);
}

function showError(msg: string) {
  readout.replaceChildren(h('div', { class: 'report' }, h('div', { class: 'error-banner' }, `⚠ ${msg}`)));
}

/* ---------- input wiring ---------- */
function fromFile(file: File | null | undefined) {
  if (!file) return;
  stopCamera();
  file.arrayBuffer().then((buf) => scanBytes(new Uint8Array(buf), file.name || 'pasted image'));
}

dropzone.addEventListener('click', () => fileInput.click());
dropzone.addEventListener('keydown', (e) => {
  const ev = e as KeyboardEvent;
  if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); fileInput.click(); }
});
fileInput.addEventListener('change', () => fromFile(fileInput.files?.[0]));

['dragenter', 'dragover'].forEach((t) =>
  dropzone.addEventListener(t, (e) => { e.preventDefault(); dropzone.classList.add('drag'); }));
dropzone.addEventListener('dragleave', (e) => {
  // dragleave bubbles from children — only deactivate when truly leaving the zone
  if (dropzone.contains((e as DragEvent).relatedTarget as Node)) return;
  e.preventDefault();
  dropzone.classList.remove('drag');
});
dropzone.addEventListener('drop', (e) => {
  e.preventDefault();
  dropzone.classList.remove('drag');
  fromFile((e as DragEvent).dataTransfer?.files?.[0]);
});

window.addEventListener('paste', (e) => {
  const items = (e as ClipboardEvent).clipboardData?.items;
  if (!items) return;
  for (const it of items) if (it.type.startsWith('image/')) { fromFile(it.getAsFile()); break; }
});

document.querySelectorAll<HTMLButtonElement>('[data-sample]').forEach((btn) =>
  btn.addEventListener('click', async () => {
    stopCamera();
    const name = btn.dataset.sample!;
    try {
      const res = await fetch(`/samples/${name}.png`);
      if (!res.ok) throw new Error(`sample ${name} not found (${res.status})`);
      scanBytes(new Uint8Array(await res.arrayBuffer()), `sample: ${name}`);
    } catch (err) { showError(String(err)); }
  }));

$('#profile').addEventListener('change', () => { if (lastBytes && !cameraOn) scanBytes(lastBytes, 'rescan'); });
$('#budget').addEventListener('change', () => { if (lastBytes && !cameraOn) scanBytes(lastBytes, 'rescan'); });
$('#alphamode').addEventListener('change', () => { if (lastBytes && !cameraOn) scanBytes(lastBytes, 'rescan'); });

/* ---------- live camera (scan_frame) ---------- */
let cameraOn = false;
let stream: MediaStream | null = null;
let rafId = 0;
let lastFrameAt = 0;

async function startCamera() {
  if (!ready) return;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: 'environment' } });
    video.srcObject = stream;
    await video.play();
    cameraOn = true;
    cameraBtn.classList.add('live');
    cameraBtn.replaceChildren(...camLive());
    previewImg.hidden = true; video.hidden = false; previewFig.hidden = false;
    cornersSvg.replaceChildren();
    loopFrame();
  } catch (err) {
    showError(`camera: ${String(err)}`);
  }
}
function stopCamera() {
  if (!cameraOn) return;
  cameraOn = false;
  cancelAnimationFrame(rafId);
  stream?.getTracks().forEach((t) => t.stop());
  stream = null;
  cameraBtn.classList.remove('live');
  cameraBtn.replaceChildren(...camIdle());
}
function loopFrame(ts = 0) {
  if (!cameraOn) return;
  rafId = requestAnimationFrame(loopFrame);
  if (ts - lastFrameAt < 180 || !video.videoWidth) return;
  lastFrameAt = ts;
  if (!frameCtx) return;
  const w = video.videoWidth, ht = video.videoHeight;
  frameCanvas.width = w; frameCanvas.height = ht;
  frameCtx.drawImage(video, 0, 0, w, ht);
  const imageData = frameCtx.getImageData(0, 0, w, ht);
  try {
    const report = scan_frame(new Uint8Array(imageData.data), w, ht, profile(), budget()) as ScanReport;
    lastDetection = report.detections[0] ?? null;
    renderReport(report, 0);
    previewCap.textContent = `live · ${w}×${ht} · profile ${profile()}`;
  } catch { /* transient frame errors ignored */ }
}
cameraBtn.addEventListener('click', () => (cameraOn ? stopCamera() : startCamera()));

boot();
