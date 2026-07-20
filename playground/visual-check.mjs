// Headless functional check of the playground — drives the real page in
// system Chrome, exercises every feature, screenshots each state.
// Run: `npm run dev` in one shell, then `node visual-check.mjs`.
import { chromium } from 'playwright';

const URL = process.env.PG_URL || 'http://localhost:5173';
const OUT = '/tmp';

const SAMPLES = [
  { name: 'clean',    expect: 'qrc-ai.com/76xMa', sym: 'qr_code',     kind: 'url' },
  { name: 'artistic', expect: 'K1Ng2',            sym: 'qr_code',     kind: 'url' },
  { name: 'rescue',   expect: 'rescue-pin',       sym: 'qr_code',     kind: 'url', rescue: true },
  // dark modules over canvas-style transparency (RGB black stored under
  // alpha 0) — the input class that was a false "no detection" pre-0.9;
  // must decode, carry the alpha block, and read a light_only envelope.
  { name: 'transparent', expect: 'qrc-ai.com/76xMa', sym: 'qr_code',  kind: 'url', alpha: 'light_only' },
  { name: 'ean13',    expect: '9506000134352',    sym: 'ean13',       kind: 'gs1' },
  { name: 'gs1',      expect: '0950600013435',    sym: 'data_matrix', kind: 'gs1' },
];

const browser = await chromium.launch({
  channel: 'chrome', headless: true,
  args: ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream'],
});
const page = await browser.newPage({ viewport: { width: 1280, height: 1120 }, deviceScaleFactor: 2 });

const errors = [];
page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
page.on('pageerror', (e) => errors.push('PAGEERROR: ' + e.message));

await page.goto(URL, { waitUntil: 'networkidle' });
await page.waitForSelector('#status[data-state="ready"]', { timeout: 20000 });
const version = (await page.textContent('#version'))?.trim();

const read = () => page.evaluate(() => {
  const txt = (s) => document.querySelector(s)?.textContent?.trim() ?? null;
  return {
    verdict: txt('.verdict-text'),
    symbology: txt('.badge.sym'),
    kind: txt('.badge.kind'),
    rescueBadge: !!document.querySelector('.badge.rescue'),
    score: txt('.score-num'),
    grade: txt('.grade-chip')?.replace(/\s+/g, ' '),
    axes: document.querySelectorAll('.axis').length,
    uec: !!document.querySelector('.module.uec'),
    uecDanger: !!document.querySelector('.module.uec.danger'),
    iso: !!document.querySelector('.iso'),
    hints: document.querySelectorAll('.hint').length,
    critHint: !!document.querySelector('.hint.crit'),
    conformant: txt('.pill-ok') || txt('.pill-no'),
    cornersDrawn: !!document.querySelector('.corners polygon'),
    rawJson: !!document.querySelector('details.raw'),
    alphaPlacement: txt('.alpha-placement'),
    alphaProbes: document.querySelectorAll('.alpha-probe').length,
  };
});

const results = [];
for (const s of SAMPLES) {
  await page.click(`[data-sample="${s.name}"]`);
  await page.waitForFunction(
    (txt) => document.querySelector('.verdict-text')?.textContent?.includes(txt),
    s.expect, { timeout: 20000 },
  );
  await page.waitForTimeout(150); // let corners + reveal settle
  const got = await read();
  await page.screenshot({ path: `${OUT}/pg-${s.name}.png`, fullPage: true });
  const ok =
    got.verdict?.includes(s.expect) && got.symbology === s.sym && got.kind === s.kind &&
    (!s.rescue || (got.rescueBadge && got.uecDanger && got.critHint)) &&
    (!s.alpha || (got.alphaPlacement === s.alpha && got.alphaProbes >= 5 && got.hints >= 1));
  results.push({ sample: s.name, ok, got });
}

// profile switching on `clean`
await page.click('[data-sample="clean"]');
await page.waitForFunction((t) => document.querySelector('.verdict-text')?.textContent?.includes(t), 'qrc-ai.com', { timeout: 20000 });
await page.check('input[name="profile"][value="fast"]');
await page.waitForTimeout(400);
const fast = await read();
await page.check('input[name="profile"][value="frame"]');
await page.waitForTimeout(400);
const frame = await read();
const profileTest = {
  fast_scored: fast.score !== null,
  frame_no_score: frame.score === null,
  frame_still_decoded: frame.verdict?.includes('qrc-ai.com'),
};
await page.check('input[name="profile"][value="full"]');

// upload path (drop / browse) via the hidden file input
await page.setInputFiles('#file', 'public/samples/artistic.png');
await page.waitForFunction((t) => document.querySelector('.verdict-text')?.textContent?.includes(t), 'K1Ng2', { timeout: 20000 });
const uploadOk = (await read()).verdict?.includes('K1Ng2');

// FNC1 (0x1D) must render as a visible control picture (␝ U+241D), never raw
await page.click('[data-sample="gs1"]');
await page.waitForFunction(() => document.querySelector('.verdict-text')?.textContent?.includes('0950600013435'), { timeout: 20000 });
const gv = (await page.textContent('.verdict-text')) ?? '';
const printableOk = gv.includes(String.fromCharCode(0x241d)) && !gv.includes(String.fromCharCode(0x1d));

// camera: a fake device feeds scan_frame live — must go live (or banner) and never crash
const errBeforeCam = errors.length;
await page.click('#camera-btn');
await page.waitForTimeout(1300); // let scan_frame run on the fake device for several frames
const camState = await page.evaluate(() => ({
  live: !!document.querySelector('.cam.live'),
  banner: !!document.querySelector('.error-banner'),
}));
const cameraGraceful = (camState.live || camState.banner) && errors.length === errBeforeCam;
if (camState.live) await page.click('#camera-btn');

await browser.close();

const extra = { uploadOk, printableOk, cameraGraceful };
console.log(JSON.stringify({ version, errors, profileTest, extra, results }, null, 2));
const allOk = results.every((r) => r.ok) && errors.length === 0 &&
  profileTest.fast_scored && profileTest.frame_no_score && profileTest.frame_still_decoded &&
  uploadOk && printableOk && cameraGraceful;
console.log(allOk ? '\nALL CHECKS PASSED ✓' : '\nSOME CHECKS FAILED ✗');
process.exit(allOk ? 0 : 1);
