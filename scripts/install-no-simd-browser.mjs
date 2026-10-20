// A pinned historical test runtime retains V8's real SIMD disable flag. It is
// used only against the local acceptance host, never as a browser recommendation.
import { mkdir, writeFile, appendFile, access } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { join, resolve } from 'node:path';
const platforms = {
  win32: { file:'chromium-win64.zip', sha256:'5ce25508313fb9fe1bc12e533c2a24934c651427024f9ecec896350ac241a9d5', executable:'chrome-win/chrome.exe' },
  linux: { file:'chromium-linux.zip', sha256:'1e7ae56e9f7b95ac5bda7a26aebd0feb4c111c1a207f6c0e462e33300e667bec', executable:'chrome-linux/chrome' },
};
const platform = platforms[process.platform];
if (!platform || process.arch !== 'x64') throw Error('This installer qualifies Linux x64 and Windows x64; supply a separately verified browser path on other hosts');
const directory = resolve(process.argv[2]); await mkdir(directory, {recursive:true});
const url = `https://cdn.playwright.dev/dbazure/download/playwright/builds/chromium/978106/${platform.file}`;
const response = await fetch(url); if (!response.ok) throw Error(`Test browser HTTP${response.status}`);
const bytes = new Uint8Array(await response.arrayBuffer());
const sha256 = createHash('sha256').update(bytes).digest('hex');
if (sha256 !== platform.sha256) throw Error('Pinned test-browser checksum mismatch');
const archive = join(directory, platform.file); await writeFile(archive, bytes);
if (process.platform === 'win32') {
  execFileSync('powershell.exe',['-NoProfile','-NonInteractive','-Command',
    'Expand-Archive -LiteralPath $env:QRSCAN_BROWSER_ZIP -DestinationPath $env:QRSCAN_BROWSER_DIR'],
    {env:{...process.env,QRSCAN_BROWSER_ZIP:archive,QRSCAN_BROWSER_DIR:directory},stdio:'pipe'});
} else execFileSync('unzip',['-q',archive,'-d',directory],{stdio:'pipe'});
const executable = join(directory, platform.executable); await access(executable);
const receipt = {url,sha256,executable,revision:'978106',scope:'Real SIMD-disabled local acceptance only'};
await writeFile(join(directory,'browser-receipt.json'),JSON.stringify(receipt,null,2)+'\n');
if (process.env.GITHUB_ENV) await appendFile(process.env.GITHUB_ENV,`QRSCAN_NO_SIMD_BROWSER=${executable}\n`);
console.log(JSON.stringify(receipt));
