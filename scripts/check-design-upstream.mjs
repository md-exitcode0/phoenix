// Review-only updater. Never executes fetched source or modifies Phoenix prompts.
import { readFile, realpath, stat } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { resolve, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
const root = fileURLToPath(new URL('../', import.meta.url));
const manifest = JSON.parse(await readFile(join(root, 'docs/iris-design-upstream.json'), 'utf8'));
const [source, ...extra] = process.argv.slice(2);
if (!source || extra.length) throw Error('Use a reviewed checkout path or --remote');
if (source === '--remote') {
  const line = execFileSync('git', ['ls-remote', '--', manifest.repository, 'HEAD'], { encoding:'utf8', timeout:20000, maxBuffer:8192 }).trim();
  const revision = line.split(/\s+/)[0];
  if (!/^[0-9a-f]{40}$/.test(revision)) throw Error('Invalid upstream revision response');
  console.log(JSON.stringify({ pinned:manifest.revision, upstream:revision, updateAvailable:revision !== manifest.revision, action:'Review changes before updating; no code was fetched or applied.' },null,2));
} else {
  const checkout = await realpath(resolve(source));
  const changes = [];
  for (const [name, expected] of Object.entries(manifest.files)) {
    if (name.startsWith('/') || name.split('/').includes('..')) throw Error('Unsafe manifest path');
    const path = await realpath(join(checkout,name));
    if (relative(checkout,path).startsWith('..')) throw Error('Reference source escaped checkout');
    const info = await stat(path);
    if (!info.isFile() || info.size > 1024*1024) throw Error('Unexpected source size/type: '+name);
    const actual = createHash('sha256').update(await readFile(path)).digest('hex');
    if (actual !== expected) changes.push({file:name,expected,actual,licenseReview:/LICENSE|NOTICE/.test(name)});
  }
  console.log(JSON.stringify({pinned:manifest.revision, checked:Object.keys(manifest.files).length, changes, applied:false},null,2));
  if (changes.length) process.exitCode=1;
}
