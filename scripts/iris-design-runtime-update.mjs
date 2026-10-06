import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const pinned = '3ee7948d8ec9d3f2ac538c7ac9b6c9fa8e345c28';
const previous = 'd567c74c2eb0fa34569f00544878ff95a90ebd44';
const previewFiles = [
  'apps/server/src/design-preview-runner.ts', 'apps/server/src/design-static-preview.ts',
  'apps/server/src/api-workspace-paths.ts', 'apps/server/src/safe-command-environment.ts',
  'packages/proc/src/cli.ts', 'packages/proc/src/desktop-path.ts', 'packages/proc/src/kill.ts',
];
const previewDependencies = [
  { name: 'zod', version: '4.4.3', integrity: 'sha512-ytENFjIJFl2UwYglde2jchW2Hwm4GJFLDiSXWdTrJQBIN9Fcyp7n4DhxJEiWNAJMV1/BqWfW/kkg71UDcHJyTQ==' },
  { name: 'parse5', version: '7.3.0', integrity: 'sha512-IInvU7fabl34qmi9gY8XOVxhYyMyuH2xUNpb2q8/Y+7552KlejkRvqvD19nMoUW/uQGGbqNpA6Tufu5FL5BZgw==' },
  { name: 'entities', version: '6.0.1', integrity: 'sha512-aN97NXWF6AWBTahfVOIrB/NShkzi5H7F9r1s9mD3cDj4Ko5f2qhhVoYMibXF7GlLveb/D2ioWay8lxI97Ven3g==' },
];

function walk(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const file = path.join(directory, entry.name);
    if (entry.isSymbolicLink()) throw new Error(`Cannot vendor symlink: ${file}`);
    return entry.isDirectory() ? walk(file) : [file];
  }).sort();
}

// Updates require an explicitly reviewed, separate checkout and a full commit.
// This script never fetches, checks out, or modifies that source checkout.
export function importRuntime(checkout, revision = pinned) {
  if (!/^[a-f0-9]{40}$/.test(revision)) throw new Error('Use a full reviewed commit SHA');
  const source = path.resolve(checkout);
  const forbidden = path.join(root, 'artifacts/iris-design-integration-2026-09-18/upstream');
  if (source === forbidden) throw new Error('Use a NEW evidence checkout, not the original checkout');
  const git = (...args) => execFileSync('git', ['-C', source, ...args], { encoding: 'utf8', maxBuffer: 4_000_000 }).trim();
  if (git('rev-parse', 'HEAD') !== revision) throw new Error('Checkout HEAD does not match reviewed revision');
  if (git('status', '--porcelain', '--', 'packages/design-agent', 'apps/server/src/orchestrator.ts', 'LICENSE', 'NOTICE', 'THIRD_PARTY_NOTICES.md'))
    throw new Error('Reviewed upstream source is dirty');
  const vendor = path.join(root, 'vendor/tastecode-design');
  if (existsSync(vendor)) throw new Error('Vendor destination exists; review its manifest and move it aside explicitly before an update');
  mkdirSync(vendor, { recursive: true });
  const packageRoot = path.join(source, 'packages/design-agent');
  const entries = git('ls-files', 'packages/design-agent').split('\n').filter(Boolean);
  const hashes = {};
  for (const relative of entries) {
    const file = path.join(source, relative);
    const destination = path.join(vendor, path.relative(packageRoot, file));
    mkdirSync(path.dirname(destination), { recursive: true });
    cpSync(file, destination, { errorOnExist: true, force: false });
    hashes[relative] = createHash('sha256').update(readFileSync(file)).digest('hex');
  }
  for (const relative of ['LICENSE', 'NOTICE', 'THIRD_PARTY_NOTICES.md', 'apps/server/src/orchestrator.ts']) {
    const target = relative.startsWith('apps/') ? path.join(vendor, 'host-reference/orchestrator.ts') : path.join(vendor, relative);
    mkdirSync(path.dirname(target), { recursive: true });
    cpSync(path.join(source, relative), target, { errorOnExist: true, force: false });
    hashes[relative] = createHash('sha256').update(readFileSync(target)).digest('hex');
  }
  const provenance = { version: 2, repository: 'https://github.com/Leonxlnx/tastecode.git', revision,
    previousRevision: previous, license: 'Apache-2.0', sourceModified: false,
    imported: hashes, changesFromPrevious: git('diff', '--stat', previous, revision, '--', 'packages/design-agent'),
    adapter: 'scripts/iris-design-runtime.mjs',
    layout: 'scripts/iris-design-runtime.mjs and vendor/tastecode-design must share the same parent root',
    update: 'Import from a separate clean checkout at an explicitly reviewed full SHA; rebuild and run deterministic tests. Never update the original checkout.',
  };
  writeFileSync(path.join(vendor, 'UPSTREAM.json'), `${JSON.stringify(provenance, null, 2)}\n`);
  writeFileSync(path.join(root, 'docs/iris-design-upstream-v2.json'), `${JSON.stringify(provenance, null, 2)}\n`);
  return { revision, importedFiles: Object.keys(hashes).length, vendorFiles: walk(vendor).length };
}

export function importPreviewRuntime(checkout, archives) {
  const source = path.resolve(checkout);
  const head = execFileSync('git', ['-C', source, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  if (head !== pinned) throw new Error('Preview source must match the pinned revision');
  const dirty = execFileSync('git', ['-C', source, 'status', '--porcelain', '--', ...previewFiles], { encoding: 'utf8' }).trim();
  if (dirty) throw new Error('Preview upstream sources are dirty');
  const vendor = path.join(root, 'vendor/tastecode-design');
  const provenance = JSON.parse(readFileSync(path.join(vendor, 'UPSTREAM.json'), 'utf8'));
  for (const relative of previewFiles) {
    const destination = relative.startsWith('packages/')
      ? path.join(vendor, 'server/proc', path.basename(relative))
      : path.join(vendor, 'server/src', path.basename(relative));
    mkdirSync(path.dirname(destination), { recursive: true });
    cpSync(path.join(source, relative), destination, { errorOnExist: true, force: false });
    provenance.imported[relative] = createHash('sha256').update(readFileSync(destination)).digest('hex');
  }
  const dependencies = [];
  for (const dependency of previewDependencies) {
    const archive = path.resolve(archives, `${dependency.name}-${dependency.version}.tgz`);
    const bytes = readFileSync(archive);
    const integrity = `sha512-${createHash('sha512').update(bytes).digest('base64')}`;
    if (integrity !== dependency.integrity) throw new Error(`Archive integrity mismatch: ${dependency.name}`);
    const destination = path.join(vendor, 'server/dependencies', dependency.name);
    if (existsSync(destination)) throw new Error(`Dependency destination already exists: ${dependency.name}`);
    mkdirSync(destination, { recursive: true });
    execFileSync('tar', ['-xzf', archive, '-C', destination, '--strip-components=1']);
    const metadata = JSON.parse(readFileSync(path.join(destination, 'package.json'), 'utf8'));
    if (metadata.name !== dependency.name || metadata.version !== dependency.version) throw new Error('Dependency identity mismatch');
    dependencies.push({ ...dependency, license: metadata.license, source: `https://registry.npmjs.org/${dependency.name}/-/${dependency.name}-${dependency.version}.tgz` });
  }
  provenance.previewDependencies = dependencies;
  writeFileSync(path.join(vendor, 'server/DEPENDENCIES.json'), `${JSON.stringify(dependencies, null, 2)}\n`);
  writeFileSync(path.join(vendor, 'UPSTREAM.json'), `${JSON.stringify(provenance, null, 2)}\n`);
  writeFileSync(path.join(root, 'docs/iris-design-upstream-v2.json'), `${JSON.stringify(provenance, null, 2)}\n`);
  return { revision: pinned, previewSourceFiles: previewFiles.length, dependencies };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if (!process.argv[2]) throw new Error('Usage: node scripts/iris-design-runtime-update.mjs NEW_CHECKOUT [REVIEWED_FULL_SHA], or --preview NEW_CHECKOUT ARCHIVE_DIRECTORY');
    const result = process.argv[2] === '--preview'
      ? importPreviewRuntime(process.argv[3], process.argv[4])
      : importRuntime(process.argv[2], process.argv[3] ?? pinned);
    process.stdout.write(`${JSON.stringify(result)}\n`);
  } catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
}
