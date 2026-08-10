import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const __dirname = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

// Load native addon for the current platform (dev builds land in rust/ or repo root)
const { platform, arch } = process;
const file = `markit.${platform}-${arch === 'x64' ? 'x64' : arch}${platform === 'linux' ? '-gnu' : ''}.node`;
const candidates = [join(__dirname, file), join(__dirname, '..', file)];
const found = candidates.find((p) => require('node:fs').existsSync(p));
if (!found) {
  console.error(`No native build found (looked for ${file}). Run: bun run build:native`);
  process.exit(1);
}
const native = require(found);

console.log('Exports:', Object.keys(native));

// Test converterNames
const names = native.converterNames();
console.log('Converter names:', names);

// Test Markit class
const m = new native.Markit();
console.log('Markit instance:', m);

// Test convert with plain text (should pass through)
const input = Buffer.from('# Hello World\n\nThis is a test.');
const result = await m.convert(input, { extension: '.md' });
console.log('Convert result:', result);
console.log('Markdown:', JSON.stringify(result.markdown));
console.log('Title:', result.title);

// Test converterAccepts
const accepts = native.converterAccepts('plain-text', { extension: '.txt' });
console.log('plain-text accepts .txt:', accepts);

// Test converterConvert
const csvInput = Buffer.from('name,age\nAlice,30\nBob,25');
const csvResult = await native.converterConvert('csv', csvInput, { extension: '.csv' });
console.log('CSV result:', csvResult.markdown.slice(0, 100));

// Test convertFile
const fs = await import('node:fs');
const tmpFile = '/tmp/markit-napi-test.md';
fs.writeFileSync(tmpFile, '# File Test\n\nWorks!');
const fileResult = await m.convertFile(tmpFile);
console.log('File result:', fileResult.markdown);
fs.unlinkSync(tmpFile);

// Test converterConvertUrl (no hook for plain-text)
const urlResult = await native.converterConvertUrl('plain-text', 'https://example.com');
console.log('converterConvertUrl (no hook):', urlResult);

console.log('\n✅ All smoke tests passed!');
