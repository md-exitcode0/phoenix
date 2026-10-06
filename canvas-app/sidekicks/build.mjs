import { build } from '../chromium-shell/node_modules/esbuild/lib/main.js';
import { mkdir, rename, writeFile } from 'node:fs/promises';
import { dirname, basename } from 'node:path';
import { fileURLToPath } from 'node:url';

const source = fileURLToPath(new URL('.', import.meta.url));
const destination = fileURLToPath(new URL('../ui/', import.meta.url));
const result = await build({
  absWorkingDir: source,
  entryPoints: { 'sidekicks-bundle': 'runtime.mjs', 'sidekick-studio': 'sidekick-studio.mjs' },
  outdir: destination,
  bundle: true,
  minify: !process.argv.includes('--dev'),
  format: 'iife',
  platform: 'browser',
  target: ['chrome120', 'safari17'],
  charset: 'utf8',
  legalComments: 'inline',
  write: false,
  logLevel: 'warning',
});

// Both entries must compile before any accepted bundle is replaced.
for (const output of result.outputFiles) {
  await mkdir(dirname(output.path), { recursive: true });
  const temporary = `${output.path}.tmp-${process.pid}`;
  await writeFile(temporary, output.contents);
  await rename(temporary, output.path);
  console.log(`${basename(output.path)}: ${(output.contents.byteLength / 1024).toFixed(1)} KiB`);
}
