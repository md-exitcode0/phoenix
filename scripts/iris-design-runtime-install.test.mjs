import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import {
  appendFileSync, chmodSync, constants, copyFileSync, cpSync, existsSync, lstatSync,
  mkdirSync, mkdtempSync, readFileSync, readdirSync, readlinkSync, renameSync, rmSync,
  symlinkSync, writeFileSync,
} from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// Execute only copies of the installer and the original frozen package. Every
// child gets a private HOME/PHOENIX_HOME; no provider or preview action is used.
const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const vendorPath = 'vendor/tastecode-design';
const helpers = ['iris-design-runtime.mjs', 'iris-design-preview.mjs'];
const hash = (value) => createHash('sha256').update(value).digest('hex');
const sourcePaths = ['install.sh', 'docs/iris-design.md', 'scripts/iris-design-runtime-install.test.mjs',
  ...helpers.map((name) => `scripts/${name}`), `${vendorPath}/MANIFEST.json`];
const sourceHashes = () => Object.fromEntries(sourcePaths.map((name) => [name, hash(readFileSync(path.join(repo, name)))]));
const manifest = JSON.parse(readFileSync(path.join(repo, vendorPath, 'MANIFEST.json'), 'utf8'));
const expectedManifest = hash(readFileSync(path.join(repo, vendorPath, 'MANIFEST.json')));
const copyTree = (from, to) => cpSync(from, to, { recursive: true, mode: constants.COPYFILE_FICLONE });
const extension = '.local/share/gnome-shell/extensions/phoenix-cursor@phoenix.dev';
let evidence;
let currentCase = 0;
const report = { manifestSha256: expectedManifest, packageFiles: manifest.files.length, cases: [] };

function write(file, value, mode = 0o600) {
  mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  writeFileSync(file, value, { mode });
}

function snapshot(root, exclude = () => false) {
  if (!existsSync(root) && !isLink(root)) return null;
  const result = {};
  function visit(file, relative) {
    if (exclude(relative)) return;
    const stat = lstatSync(file);
    if (stat.isSymbolicLink()) result[relative] = { link: readlinkSync(file) };
    else if (stat.isDirectory()) {
      result[relative] = { mode: stat.mode & 0o7777, directory: true };
      for (const name of readdirSync(file).sort()) visit(path.join(file, name), relative ? `${relative}/${name}` : name);
    } else {
      assert.ok(stat.isFile(), `Unexpected fixture path type: ${file}`);
      result[relative] = { mode: stat.mode & 0o7777, bytes: stat.size, sha256: hash(readFileSync(file)), inode: stat.ino };
    }
  }
  visit(root, '');
  return result;
}

function isLink(file) { try { return lstatSync(file).isSymbolicLink(); } catch { return false; } }
function protectedState(f) {
  const exclude = (relative) => /(?:^|\/)(?:design-runtime(?:\.prev)?|\.design-runtime\.[^/]+)(?:\/|$)/.test(relative);
  return { home: snapshot(f.home, exclude), state: f.custom ? snapshot(f.state, exclude) : null };
}
function assertProtected(f) { assert.deepEqual(protectedState(f), f.protected); }
function assertNoForbiddenCommands(f) { assert.equal(existsSync(f.forbidden), false, 'Unexpected full-install command'); }
function scratchNames(f) { return readdirSync(f.state).filter((name) => name.startsWith('.design-runtime.') || name.startsWith('.phoenix.exchange-probe.')); }

function fixture({ custom = false, packagePresent = true, pristine = false } = {}) {
  const root = mkdtempSync(path.join(evidence, 'private-'));
  chmodSync(root, 0o700);
  const f = { root, custom, repo: path.join(root, 'source with spaces'), home: path.join(root, 'home'),
    tmp: path.join(root, 'tmp'), bin: path.join(root, 'guard-bin'), outside: path.join(root, 'outside'), invocation: 0 };
  f.state = custom ? path.join(root, 'custom private state') : path.join(f.home, '.phoenix');
  f.dest = path.join(f.state, 'design-runtime');
  f.forbidden = path.join(root, 'forbidden.log');
  for (const directory of [f.repo, f.home, f.tmp, f.bin, f.outside]) mkdirSync(directory, { recursive: true, mode: 0o700 });
  copyFileSync(path.join(repo, 'install.sh'), path.join(f.repo, 'install.sh'));
  if (packagePresent) {
    copyTree(path.join(repo, vendorPath), path.join(f.repo, vendorPath));
    for (const helper of helpers) {
      mkdirSync(path.join(f.repo, 'scripts'), { recursive: true, mode: 0o700 });
      copyFileSync(path.join(repo, 'scripts', helper), path.join(f.repo, 'scripts', helper));
    }
  }
  // Sources deliberately conflict with saved overlays, templates and extension.
  write(path.join(f.repo, 'prompts/frontend_system.md'), 'replacement Iris source\n');
  write(path.join(f.repo, 'prompts/coordinator_system.md'), 'replacement coordinator\n');
  write(path.join(f.repo, 'templates/example.txt'), 'replacement template\n');
  write(path.join(f.repo, 'desktop/gnome-extension/phoenix-cursor@phoenix.dev/metadata.json'), '{"source":true}\n');
  for (const command of ['cargo', 'systemd-run', 'systemctl', 'gnome-extensions']) {
    write(path.join(f.bin, command), `#!/bin/sh\nprintf '%s\\n' '${command}' >> "$IRIS_INSTALL_TEST_FORBIDDEN"\nexit 92\n`, 0o700);
  }
  if (!pristine) {
    for (const [file, content] of Object.entries({
      'prompts/frontend_system.md': 'saved Iris prompt sentinel\n',
      'prompts/coordinator_system.md': 'saved coordinator prompt sentinel\n',
      'prompts/custom_system.md': 'custom role must survive stale cleanup\n',
      'prompts/revisions.json': '{"revision":17}\n',
      'prompts.prev/frontend_system.md': 'prior prompt sentinel\n',
      'templates/example.txt': 'saved template\n',
      'templates.prev/example.txt': 'prior template\n',
      'company-state.json': '{"preserve":true}\n',
    })) write(path.join(f.state, file), content);
    write(path.join(f.home, '.local/bin/phoenix'), 'installed binary sentinel\n', 0o700);
    write(path.join(f.home, '.local/bin/phoenix.prev'), 'prior binary sentinel\n', 0o700);
    write(path.join(f.home, extension, 'metadata.json'), '{"saved":true}\n');
    write(path.join(f.home, `${extension}.prev`, 'metadata.json'), '{"prior":true}\n');
    if (custom) write(path.join(f.home, '.phoenix/prompts/frontend_system.md'), 'default-root sentinel\n');
  }
  f.env = { HOME: f.home, TMPDIR: f.tmp, PHOENIX_HOME: custom ? f.state : '', PHOENIX_NODE: process.execPath,
    PHOENIX_INSTALL_NO_SYSTEMD_RUN: '1', WAYLAND_DISPLAY: 'fixture-wayland',
    PATH: [f.bin, path.dirname(process.execPath), '/usr/bin', '/bin'].join(path.delimiter),
    IRIS_INSTALL_TEST_FORBIDDEN: f.forbidden, LANG: 'C.UTF-8' };
  f.protected = protectedState(f);
  return f;
}

function install(f, args = ['--design-runtime-only'], env = {}) {
  assert.ok(f.env.HOME.startsWith(`${f.root}${path.sep}`));
  const result = spawnSync('/bin/bash', ['./install.sh', ...args], {
    cwd: f.repo, env: { ...f.env, ...env }, encoding: 'utf8', timeout: 25000, maxBuffer: 2000000,
  });
  const name = `case-${currentCase}-install-${++f.invocation}`;
  write(path.join(evidence, `${name}.stdout.log`), result.stdout ?? '');
  write(path.join(evidence, `${name}.stderr.log`), result.stderr ?? '');
  write(path.join(evidence, `${name}.json`), JSON.stringify({ args, status: result.status, signal: result.signal, error: result.error?.message }));
  assert.ifError(result.error);
  assert.equal(result.signal, null);
  assert.ok(existsSync(path.join(f.repo, 'install.sh')), 'Installer must preserve its copied source after failure');
  return { ...result, output: `${result.stdout}${result.stderr}` };
}

function verifyInstalled(f, root = f.dest) {
  assert.ok(root.startsWith(`${f.root}${path.sep}`));
  const result = spawnSync(process.execPath, [path.join(root, 'scripts/iris-design-runtime.mjs')], {
    cwd: root, env: f.env, input: JSON.stringify({ action: 'check', full: true, expectedManifestSha256: expectedManifest }),
    encoding: 'utf8', timeout: 10000, maxBuffer: 2000000,
  });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${result.stdout}${result.stderr}`);
  const checked = JSON.parse(result.stdout);
  assert.equal(checked.ok, true);
  assert.equal(checked.manifestSha256, expectedManifest);
  assert.equal(checked.verified, manifest.files.length);
  write(path.join(evidence, `case-${currentCase}-installed-check.json`), `${JSON.stringify(checked)}\n`);
}

function originalInstall(f) {
  copyTree(path.join(f.repo, vendorPath), path.join(f.dest, vendorPath));
  mkdirSync(path.join(f.dest, 'scripts'), { recursive: true, mode: 0o700 });
  for (const helper of helpers) copyFileSync(path.join(f.repo, 'scripts', helper), path.join(f.dest, 'scripts', helper));
  write(path.join(f.dest, 'previous-install.txt'), 'original checked package at destination\n');
  write(path.join(`${f.dest}.prev`, 'older-backup.txt'), 'previous backup sentinel\n');
  return { active: snapshot(f.dest), prior: snapshot(`${f.dest}.prev`) };
}

function failure(result, message) { assert.notEqual(result.status, 0, result.output); assert.match(result.output, message); }
function preserveFailedInstall(f, before) {
  assert.deepEqual(snapshot(f.dest), before.active);
  assert.deepEqual(snapshot(`${f.dest}.prev`), before.prior);
  assert.deepEqual(scratchNames(f), []);
  assertProtected(f);
  assertNoForbiddenCommands(f);
}

async function runCase(t, name, options, body) {
  await t.test(name, { timeout: 60000 }, async () => {
    currentCase += 1;
    const f = fixture(options);
    const entry = { name, fixture: f.root, status: 'failed' };
    report.cases.push(entry);
    try { await body(f); entry.status = 'passed'; }
    catch (error) { entry.error = error.message; throw error; }
    finally { if (entry.status === 'passed') rmSync(f.root, { recursive: true, force: true }); }
  });
  // A failed case retains its actual package/home for inspection. Stop before
  // allocating further complete copies on the shared machine.
  if (report.cases.at(-1)?.status === 'failed') throw new Error(`Stopped after failed installer case: ${name}`);
}

test('runtime-only installation in isolated private homes', { timeout: 240000 }, async (t) => {
  const evidenceParent = path.join(repo, 'artifacts/iris-design-mode-2026-09-20/engine');
  mkdirSync(evidenceParent, { recursive: true });
  evidence = mkdtempSync(path.join(evidenceParent, 'installer-only-'));
  chmodSync(evidence, 0o700);
  const beforeSources = sourceHashes();
  t.after(() => {
    report.sourceHashes = sourceHashes();
    report.sourceFilesUnchangedDuringTests = JSON.stringify(report.sourceHashes) === JSON.stringify(beforeSources);
    report.passed = report.cases.filter((entry) => entry.status === 'passed').length;
    report.failed = report.cases.length - report.passed;
    write(path.join(evidence, 'report.json'), `${JSON.stringify(report, null, 2)}\n`);
    console.log(`Installer evidence: ${path.relative(repo, evidence)}`);
    assert.deepEqual(report.sourceHashes, beforeSources, 'Tests must leave the real source/package untouched');
  });

  await runCase(t, 'help and incompatible full-install arguments do not mutate the home', { packagePresent: false }, (f) => {
    const help = install(f, ['--help']);
    assert.equal(help.status, 0); assert.match(help.output, /--design-runtime-only/);
    for (const args of [
      ['--design-runtime-only', '--jobs', '2'], ['-j', '2', '--design-runtime-only'],
      ['--design-runtime-only', path.join(f.root, 'unused-binary-target')],
    ]) failure(install(f, args), /cannot be combined/);
    assert.equal(existsSync(path.join(f.root, 'unused-binary-target')), false);
    assertProtected(f); assertNoForbiddenCommands(f);
  });

  await runCase(t, 'fresh home gains only the complete runtime with owner-only directories', { pristine: true }, (f) => {
    assert.equal(existsSync(f.state), false);
    assert.equal(install(f).status, 0);
    assert.deepEqual(readdirSync(f.home), ['.phoenix']);
    assert.deepEqual(readdirSync(f.state), ['design-runtime']);
    for (const directory of [f.state, f.dest, path.join(f.dest, 'scripts'), path.join(f.dest, 'vendor')])
      assert.equal(lstatSync(directory).mode & 0o777, 0o700);
    verifyInstalled(f); assertNoForbiddenCommands(f);
  });

  await runCase(t, 'saved prompts, binary, templates and extension remain byte-for-byte unchanged', {}, (f) => {
    assert.equal(existsSync(path.join(f.repo, 'Cargo.toml')), false);
    assert.equal(existsSync(path.join(f.repo, 'target/release/phoenix')), false);
    assert.equal(install(f, ['--design-runtime-only'], { PHOENIX_INSTALL_JOBS: 'unused-invalid-value' }).status, 0);
    verifyInstalled(f); assertProtected(f); assertNoForbiddenCommands(f);
    assert.deepEqual(scratchNames(f), []);
  });

  await runCase(t, 'custom private state upgrades atomically and retains the original package as prev', { custom: true }, (f) => {
    const before = originalInstall(f);
    assert.equal(install(f).status, 0);
    verifyInstalled(f);
    assert.deepEqual(snapshot(`${f.dest}.prev`), before.active);
    assert.equal(existsSync(path.join(f.dest, 'previous-install.txt')), false);
    assert.deepEqual(scratchNames(f), []);
    assertProtected(f); assertNoForbiddenCommands(f);
  });

  await runCase(t, 'missing package, manifest, reference or helper preserves the installed original', {}, (f) => {
    const before = originalInstall(f);
    const image = manifest.files.find((entry) => /references\/.*\.png$/.test(entry.path)).path;
    for (const relative of [vendorPath, `${vendorPath}/MANIFEST.json`, image, 'scripts/iris-design-preview.mjs']) {
      const source = path.join(f.repo, relative);
      const held = path.join(f.root, 'held-source');
      renameSync(source, held);
      try { failure(install(f), /source (?:directory|file) is missing|package integrity check failed/); }
      finally { renameSync(held, source); }
      preserveFailedInstall(f, before);
    }
  });

  await runCase(t, 'tampered executable, reference or helper fails actual package validation', {}, (f) => {
    const before = originalInstall(f);
    const image = manifest.files.find((entry) => /references\/.*\.png$/.test(entry.path)).path;
    for (const relative of [`${vendorPath}/dist/index.js`, image, 'scripts/iris-design-preview.mjs']) {
      const source = path.join(f.repo, relative), original = readFileSync(source);
      appendFileSync(source, '\n// isolated tamper test\n');
      try { failure(install(f), /package integrity check failed/); }
      finally { writeFileSync(source, original); }
      preserveFailedInstall(f, before);
    }
  });

  await runCase(t, 'source file and parent-directory symlinks are rejected without following them', {}, (f) => {
    const before = originalInstall(f);
    for (const relative of [`${vendorPath}/dist/index.js`, vendorPath, 'scripts', 'scripts/iris-design-preview.mjs']) {
      const source = path.join(f.repo, relative), held = path.join(f.outside, 'held-source');
      renameSync(source, held); symlinkSync(held, source);
      const outsideBefore = snapshot(f.outside);
      try {
        failure(install(f), /symlink|unsupported path type/);
        assert.deepEqual(snapshot(f.outside), outsideBefore);
      } finally { rmSync(source); renameSync(held, source); }
      preserveFailedInstall(f, before);
    }
  });

  await runCase(t, 'symlinked state, runtime and backup slots preserve every original target', {}, (f) => {
    const before = originalInstall(f);
    for (const source of [f.state, f.dest, `${f.dest}.prev`]) {
      const held = path.join(f.outside, 'held-destination');
      renameSync(source, held); symlinkSync(held, source);
      const outsideBefore = snapshot(f.outside);
      try {
        failure(install(f), /symlink/);
        assert.deepEqual(snapshot(f.outside), outsideBefore);
      } finally { rmSync(source); renameSync(held, source); }
      preserveFailedInstall(f, before);
    }
  });

  await runCase(t, 'failed stage allocation cannot turn an empty cleanup path into the source directory', {}, (f) => {
    const before = originalInstall(f);
    write(path.join(f.bin, 'mktemp'), `#!${process.execPath}\n` + `
import { spawnSync } from 'node:child_process';
const args = process.argv.slice(2);
if (args.some((arg) => arg.includes('/.design-runtime.stage.'))) {
  process.stderr.write('isolated staging allocation failure\\n'); process.exit(91);
}
process.exit(spawnSync('/usr/bin/mktemp', args, { stdio: 'inherit' }).status ?? 1);
`, 0o700);
    failure(install(f), /isolated staging allocation failure/);
    assert.equal(hash(readFileSync(path.join(f.repo, 'install.sh'))), hash(readFileSync(path.join(repo, 'install.sh'))));
    preserveFailedInstall(f, before);
  });

  await runCase(t, 'tampering after copy fails staged verification and removes the owned stage', {}, (f) => {
    const before = originalInstall(f);
    const wrapper = path.join(f.bin, 'stage-node');
    write(wrapper, `#!${process.execPath}\n` + `
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
const args = process.argv.slice(2);
if (args[0]?.includes('/.design-runtime.stage.')) {
  appendFileSync(path.join(path.dirname(args[0]), '../vendor/tastecode-design/dist/index.js'), '\\n// staged corruption\\n');
  writeFileSync(process.env.IRIS_INSTALL_TEST_OBSERVED, 'actual staged validation reached');
}
const result = spawnSync(${JSON.stringify(process.execPath)}, args, { input: args[0] === '-e' ? undefined : readFileSync(0), stdio: ['pipe', 'inherit', 'inherit'] });
process.exit(result.status ?? 1);
`, 0o700);
    const observed = path.join(f.root, 'stage-observed');
    failure(install(f, ['--design-runtime-only'], { PHOENIX_NODE: wrapper, IRIS_INSTALL_TEST_OBSERVED: observed }), /Staged Iris design package failed verification/);
    assert.equal(existsSync(observed), true);
    preserveFailedInstall(f, before);
  });

  for (const mode of ['exchange-after', 'backup-after']) {
    await runCase(t, `ambiguous ${mode} failure retains old package and prior backup for recovery`, {}, (f) => {
      const before = originalInstall(f);
      const observed = path.join(f.root, 'publish-observed');
      write(path.join(f.bin, 'mv'), `#!${process.execPath}\n` + `
import { spawnSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import path from 'node:path';
const args = process.argv.slice(2), source = path.basename(args.at(-2) ?? ''), dest = path.basename(args.at(-1) ?? '');
const result = spawnSync('/usr/bin/mv', args, { stdio: 'inherit' });
if (result.status === 0 && source.startsWith('.design-runtime.stage.') && dest === ${JSON.stringify(mode === 'exchange-after' ? 'design-runtime' : 'design-runtime.prev')}) {
  writeFileSync(process.env.IRIS_INSTALL_TEST_OBSERVED, 'rename completed before reported failure');
  process.exit(97);
}
process.exit(result.status ?? 1);
`, 0o700);
      failure(install(f, ['--design-runtime-only'], { IRIS_INSTALL_TEST_OBSERVED: observed }), /outcome was ambiguous/);
      assert.equal(existsSync(observed), true);
      verifyInstalled(f);
      const names = scratchNames(f);
      const prior = names.find((name) => name.startsWith('.design-runtime.prev.prior.'));
      assert.ok(prior); assert.deepEqual(snapshot(path.join(f.state, prior)), before.prior);
      if (mode === 'exchange-after') {
        const stage = names.find((name) => name.startsWith('.design-runtime.stage.'));
        assert.ok(stage); assert.deepEqual(snapshot(path.join(f.state, stage)), before.active);
      } else assert.deepEqual(snapshot(`${f.dest}.prev`), before.active);
      assertProtected(f); assertNoForbiddenCommands(f);
    });
  }

  await runCase(t, 'default full install still requires its build before publishing', { packagePresent: false }, (f) => {
    write(path.join(f.repo, 'Cargo.toml'), '[package]\nname="fixture"\nversion="0.1.0"\n');
    failure(install(f, []), /build failed/);
    // The fixture guard records/refuses the build; the real Cargo executable is
    // never run. Runtime-only cases require this log to remain absent.
    assert.equal(readFileSync(f.forbidden, 'utf8'), 'cargo\n');
    assertProtected(f);
    assert.equal(existsSync(f.dest), false);
  });
});
