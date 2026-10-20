// Qualify the packed package in a fresh Vite host, never a workspace link.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const [archiveArg, outputArg] = process.argv.slice(2);
assert.ok(archiveArg && outputArg, 'usage: packed-wasm-browser.mjs ARCHIVE.tgz RECEIPT.json');
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const archive = resolve(archiveArg), output = resolve(outputArg);
const consumer = realpathSync(mkdtempSync(join(tmpdir(), 'qrscan-packed-browser-')));
cpSync(join(root, 'tests/wasm-browser'), consumer, { recursive: true });
const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
const command = (args) => execFileSync(npm, args, { cwd: consumer, stdio: 'pipe', timeout: 180_000,
  env: { ...process.env, npm_config_cache: join(consumer, '.npm-cache') }, shell: process.platform === 'win32' });
command(['ci', '--ignore-scripts', '--no-audit', '--no-fund']);
command(['install', '--no-save', '--package-lock=false', '--ignore-scripts', '--no-audit', '--no-fund', archive]);
const require = createRequire(join(consumer, 'package.json'));
const { chromium, firefox } = require('playwright');
if (process.env.QRSCAN_INSTALL_BROWSERS === '1') {
  execFileSync(process.execPath, [join(dirname(require.resolve('playwright/package.json')), 'cli.js'), 'install',
    ...(process.platform === 'linux' ? ['--with-deps'] : []), 'chromium',
    ...(process.env.QRSCAN_TEST_FIREFOX === '1' ? ['firefox'] : [])], { cwd: consumer, stdio: 'pipe', timeout: 240_000 });
}
const vite = await import(pathToFileURL(require.resolve('vite')));
const manifest = JSON.parse(readFileSync(join(consumer, 'node_modules/@supernovae-st/qrcode-ai-scanner-wasm/package.json')));
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const fixtures = [
  ['clean', 'fixtures/clean/OK_68ms_100_4e875a2c.png', 'clean-url'],
  ['artistic', 'fixtures/artistic/blob-style-monkey-logo.webp', 'artistic-blob'],
  ['difficult', 'fixtures/degraded/logo-occluded-rescue.png', 'rescued-occluded'],
  ['unreadable', 'fixtures/degraded/FAIL_1491ms_0_584c998c.png', 'no-detection'],
  ['eci', 'fixtures/eci/eci4-a1.png', 'eci-iso-8859-2'],
  ['binary', 'fixtures/eci/eci899-binary.png', 'eci-undecodable'],
  ['barcode', 'fixtures/symbology/ean13.png', 'ean13-retail-gtin'],
].map(([name, file, golden]) => ({ name, file, bytes: [...readFileSync(join(root, file))],
  expected: JSON.parse(readFileSync(join(root, `spec/examples/${golden}.json`))).detections.map(d => d.content) }));
const normalize = value => Array.isArray(value) ? value.map(normalize) : value && typeof value === 'object'
  ? Object.fromEntries(Object.entries(value).filter(([key]) => !['ms', 'total_ms', 'elapsed_ms'].includes(key)).map(([key, child]) => [key, normalize(child)])) : value;
const passed = [], runs = [];
// SSR import must not compile WASM, fetch assets or depend on window/self.
const ssr = execFileSync(process.execPath, ['--input-type=module', '-e', `
  import assert from 'node:assert/strict';
  globalThis.WebAssembly = undefined;
  globalThis.fetch = () => { throw Error('unexpected SSR fetch'); };
  const mod = await import('@supernovae-st/qrcode-ai-scanner-wasm/auto');
  assert.deepEqual(mod.scannerCapabilities(), {wasm:false,simd:false,baseline:false});
  await assert.rejects(mod.loadScanner(), e => e.code === 'wasm_unavailable');
  console.log('SSR import safe');
`], { cwd: consumer, encoding: 'utf8' }).trim();
assert.equal(ssr, 'SSR import safe'); passed.push('Packed SSR import does not require WebAssembly or fetch assets');
execFileSync(process.execPath, [require.resolve('typescript/bin/tsc'), '--noEmit', '--strict', '--lib', 'ES2022,DOM',
  '--moduleResolution', 'bundler', '--module', 'ESNext', '--target', 'ES2022', 'typecheck.ts'], { cwd: consumer, stdio: 'pipe' });
passed.push('Installed root and auto declarations typecheck');
await vite.build({ root: consumer, logLevel: 'warn', build: { assetsInlineLimit: 0, target: 'es2020' }, worker: { format: 'es' } });
const server = await vite.preview({ root: consumer, logLevel: 'warn', preview: { host: '127.0.0.1', port: 0 } });
const base = `http://127.0.0.1:${server.httpServer.address().port}`;
const runtimes = [{ name: 'chromium', browserType: chromium, disabled: false, options: {} }];
if (process.env.QRSCAN_NO_SIMD_BROWSER) runtimes.push({ name: 'chromium-simd-disabled', options: {
  executablePath: process.env.QRSCAN_NO_SIMD_BROWSER, args: ['--js-flags=--noexperimental-wasm-simd'] }, browserType: chromium, disabled: true });
if (process.env.QRSCAN_TEST_FIREFOX === '1') {
  runtimes.push({name: 'firefox', browserType: firefox, disabled: false, options: {}});
}
try {
  for (const runtime of runtimes) {
    const browser = await runtime.browserType.launch({ headless: true, timeout: 30_000, ...runtime.options });
    try {
      const page = await browser.newPage(); const requests = [], errors = [];
      page.on('request', request => { if (/\.wasm(?:\?|$)/.test(request.url())) requests.push(request.url()); });
      page.on('pageerror', error => errors.push(error.message));
      await page.goto(base); await page.waitForFunction(() => !!window.harness);
      const capabilities = await page.evaluate(() => window.harness.scannerCapabilities());
      assert.equal(capabilities.wasm, true); assert.equal(capabilities.baseline, true);
      const disabled = runtime.disabled;
      assert.equal(capabilities.simd, !disabled, 'The real runtime, not a mocked detector, must have the expected capability');
      const selected = disabled ? 'compatible' : 'simd';
      const initStarted = performance.now();
      const init = await page.evaluate(async () => { window.autoWorker = window.harness.startWorker(); return Promise.all(Array.from({length:4}, () => window.autoWorker.request())); });
      const initializationWallMs = performance.now() - initStarted;
      assert.equal(requests.length, 1, 'Concurrent worker init fetches exactly one binary');
      for (const result of init) { assert.equal(result.ok, true); assert.equal(result.variant, selected); assert.deepEqual(result.capabilities, capabilities); assert.equal(result.version, manifest.version); }
      const reports = [], latency = [];
      for (const fixture of fixtures) {
        const started = performance.now();
        const response = await page.evaluate(bytes => window.autoWorker.request({ bytes }), fixture.bytes);
        assert.equal(response.ok, true, `${fixture.name}: ${JSON.stringify(response)}`);
        assert.deepEqual(response.result.detections.map(d => d.content), fixture.expected, fixture.name);
        assert.equal(response.result.trace.budget_exhausted, false);
        assert.deepEqual(response.result.versions, { scanner: manifest.version, pipeline: 3, score_contract: 5 });
        reports.push(response.result); latency.push({ fixture: fixture.name, wallMs: performance.now() - started });
      }
      if (!disabled) {
        const compatibleLatency = [];
        await page.evaluate(() => { window.compatibleWorker = window.harness.startWorker('compatible'); });
        const warm = await page.evaluate(() => window.compatibleWorker.request());
        assert.equal(warm.ok, true); assert.equal(warm.variant, 'compatible');
        for (let index = 0; index < fixtures.length; index++) {
          const started = performance.now();
          const response = await page.evaluate(bytes => window.compatibleWorker.request({ bytes }), fixtures[index].bytes);
          assert.equal(response.ok, true); assert.equal(response.variant, 'compatible');
          assert.deepEqual(normalize(response.result), normalize(reports[index]), `${fixtures[index].name}: functional parity excludes only timings`);
          compatibleLatency.push({ fixture: fixtures[index].name, wallMs: performance.now() - started });
        }
        latency.push({ variant: 'compatible', cases: compatibleLatency });
        passed.push('SIMD and compatible reports agree on exact content and every non-timing field across seven fixture classes');
      } else {
        const forced = await page.evaluate(async () => window.harness.startWorker('simd').request());
        assert.equal(forced.ok, false); assert.equal(forced.code, 'simd_unavailable'); assert.equal(forced.phase, 'initialization');
      }
      const main = await page.evaluate(() => window.harness.mainLoad());
      assert.equal(main.samePromise, true); assert.equal(main.variant, selected);
      const scanError = await page.evaluate(() => window.autoWorker.request({ bytes: [1,2,3] }));
      assert.equal(scanError.ok, false); assert.equal(scanError.phase, 'scan'); assert.match(scanError.message, /QRS-001/);
      await page.context().route('**/missing.wasm', route => route.fulfill({status:404,body:'Missing test asset'}));
      const missing = await page.evaluate(async () => {
        const worker = window.harness.startWorker('auto', new URL('/missing.wasm', location.href).href);
        return Promise.all([worker.request(), worker.request(), worker.request()]);
      });
      for (const result of missing) { assert.equal(result.ok, false); assert.equal(result.code, 'asset_http_failed'); assert.equal(result.status, 404); assert.equal(result.phase, 'initialization'); }
      await page.context().route('**/network-failure.wasm', route => route.abort());
      const network = await page.evaluate(() => window.harness.startWorker('auto', new URL('/network-failure.wasm', location.href).href).request());
      assert.equal(network.ok, false); assert.equal(network.code, 'asset_network_failed');
      const invalid = await page.evaluate(() => window.harness.startWorker('auto', 'data:application/wasm;base64,AAAA').request());
      assert.equal(invalid.ok, false); assert.equal(invalid.code, 'binary_invalid'); assert.equal(invalid.nativeError.name, 'CompileError');
      assert.deepEqual(errors, []);
      await page.evaluate(() => window.harness.stop());
      runs.push({ runtime: runtime.name, browser: await browser.version(), options: runtime.options, capabilities, selected, initializationWallMs, latency,
        reports: reports.map((report,index) => ({ fixture: fixtures[index].name, normalizedSha256: sha(JSON.stringify(normalize(report))) })), requests, errors });
      passed.push(`${runtime.name}: real worker/main capability selection, exact seven-fixture decoding, one initial asset, concurrent init and distinct failures`);
    } finally { await browser.close(); }
  }
} finally { await new Promise(resolveClose => server.httpServer.close(resolveClose)); }
mkdirSync(dirname(output), { recursive: true });
const receipt = { checkedAt: new Date().toISOString(), consumer, artifact: { archive, sha256: sha(readFileSync(archive)), name: manifest.name, version: manifest.version },
  passed, runs, fixtures: fixtures.map(({name,file,bytes}) => ({name,file,sha256:sha(new Uint8Array(bytes))})),
  unverified: [...(!process.env.QRSCAN_NO_SIMD_BROWSER ? ['Actual SIMD-unavailable browser not supplied'] : []), 'Exact reported Windows10/Chrome109 and Windows10/Firefox158', 'Real Safari and physical phones', 'Production app and customer exports'] };
writeFileSync(output, JSON.stringify(receipt, null, 2) + '\n'); console.log(JSON.stringify({ output, consumer, passed: passed.length, runs: runs.map(({runtime,browser,selected}) => ({runtime,browser,selected})) }));
