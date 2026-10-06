import { createHash } from 'node:crypto';
import { existsSync, lstatSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const vendor = path.join(root, 'vendor/tastecode-design');
const require = createRequire(import.meta.url);

function files(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const target = path.join(directory, entry.name);
    if (entry.isSymbolicLink()) throw new Error(`Unexpected vendor symlink: ${target}`);
    return entry.isDirectory() ? files(target) : [target];
  }).sort();
}

export async function buildRuntime() {
  const provenance = JSON.parse(readFileSync(path.join(vendor, 'UPSTREAM.json'), 'utf8'));
  // Fail before generating a new manifest if a vendored original was edited.
  // A reviewed update must regenerate provenance from its clean pinned checkout.
  for (const [upstreamPath, expected] of Object.entries(provenance.imported)) {
    const relative = upstreamPath.startsWith('packages/design-agent/')
      ? upstreamPath.slice('packages/design-agent/'.length)
      : upstreamPath === 'apps/server/src/orchestrator.ts' ? 'host-reference/orchestrator.ts'
        : upstreamPath.startsWith('apps/server/src/') ? `server/src/${path.basename(upstreamPath)}`
          : upstreamPath.startsWith('packages/proc/src/') ? `server/proc/${path.basename(upstreamPath)}`
            : upstreamPath;
    if (createHash('sha256').update(readFileSync(path.join(vendor, relative))).digest('hex') !== expected)
      throw new Error(`Vendored upstream source changed: ${upstreamPath}`);
  }
  const esbuildPath = process.env.IRIS_ESBUILD_PATH
    ?? path.join(root, 'canvas-app/chromium-shell/node_modules/esbuild');
  if (!existsSync(esbuildPath)) throw new Error('esbuild is unavailable; set IRIS_ESBUILD_PATH to an existing installation');
  const esbuild = require(esbuildPath);
  const entries = files(path.join(vendor, 'src')).filter((file) => file.endsWith('.ts'));
  // Keep one ESM module per upstream source file. Its relative imports and
  // import.meta.url still resolve ../references exactly as upstream intended.
  await esbuild.build({
    entryPoints: entries,
    outbase: path.join(vendor, 'src'),
    outdir: path.join(vendor, 'dist'),
    bundle: false,
    platform: 'node',
    format: 'esm',
    target: 'node22',
    sourcemap: false,
    legalComments: 'inline',
    logLevel: 'silent',
  });
  if (existsSync(path.join(vendor, 'server/src/design-preview-runner.ts'))) {
    const dependencyRoot = path.join(vendor, 'server/dependencies');
    const resolveDependency = (name, subpath = '') => {
      const directory = path.join(dependencyRoot, name);
      const metadata = JSON.parse(readFileSync(path.join(directory, 'package.json'), 'utf8'));
      const entry = metadata.exports?.[subpath ? `./${subpath}` : '.'];
      const imported = entry?.import;
      return path.join(directory, typeof imported === 'string' ? imported : imported?.default ?? metadata.module ?? metadata.main);
    };
    await esbuild.build({
      entryPoints: [path.join(vendor, 'server/src/design-preview-runner.ts')],
      outfile: path.join(vendor, 'server/dist/preview.js'),
      bundle: true, platform: 'node', format: 'esm', target: 'node22',
      legalComments: 'inline', logLevel: 'silent',
      alias: {
        '@harness/proc/cli': path.join(vendor, 'server/proc/cli.ts'),
        '@harness/proc/desktop-path': path.join(vendor, 'server/proc/desktop-path.ts'),
        zod: resolveDependency('zod'),
        parse5: resolveDependency('parse5'),
        entities: resolveDependency('entities'),
        'entities/decode': resolveDependency('entities', 'decode'),
        'entities/escape': resolveDependency('entities', 'escape'),
      },
    });
  }
  const imported = files(vendor).filter((file) => path.basename(file) !== 'MANIFEST.json');
  const runtimeFiles = ['iris-design-runtime.mjs', 'iris-design-preview.mjs']
    .map((name) => path.join(root, 'scripts', name)).filter((file) => existsSync(file));
  const records = [...imported, ...runtimeFiles].sort().map((file) => {
    if (!lstatSync(file).isFile()) throw new Error(`Expected regular file: ${file}`);
    const bytes = readFileSync(file);
    return { path: path.relative(root, file).split(path.sep).join('/'), bytes: bytes.length,
      sha256: createHash('sha256').update(bytes).digest('hex') };
  });
  const manifest = { version: 1, upstreamRevision: provenance.revision,
    build: { tool: 'esbuild', version: esbuild.version, format: 'esm', bundled: false, target: 'node22' },
    files: records };
  writeFileSync(path.join(vendor, 'MANIFEST.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  return { files: records.length, bytes: records.reduce((sum, entry) => sum + entry.bytes, 0),
    modules: entries.length, upstreamRevision: provenance.revision };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { process.stdout.write(`${JSON.stringify(await buildRuntime())}\n`); }
  catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
}
