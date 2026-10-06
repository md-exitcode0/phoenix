import { realpathSync } from 'node:fs';
import childProcess from 'node:child_process';
import { syncBuiltinESMExports } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { checkPackage, MAX_WIRE_BYTES, RuntimeError, runtimeRoot } from './iris-design-runtime.mjs';

// One owned service, one preview. The native owner keeps stdin open for the
// preview's lifetime and closes it to stop the exact server this helper started.
export async function previewService(input = process.stdin, output = process.stdout) {
  let buffer = Buffer.alloc(0);
  let started = false;
  let ending = false;
  let starting;
  let preview;
  let stopping;
  let errorSent = false;
  let trackingStartup = false;
  const startupChildren = new Set();
  const originalSpawn = childProcess.spawn;
  let resolveDone;
  const done = new Promise((resolve) => { resolveDone = resolve; });

  const emit = (value) => output.write(`${JSON.stringify(value)}\n`);
  const report = (error) => {
    if (errorSent) return;
    errorSent = true;
    const message = String(error?.message ?? error).slice(0, 100000);
    emit({ ready: false, error: { code: error?.code ?? 'PREVIEW_FAILED', message } });
    process.exitCode = 1;
  };
  const cancelStartupChild = (child) => {
    // Only handles returned by the unedited runner's own detached spawn are
    // eligible. Never derive a group from a caller-supplied or discovered PID.
    if (process.platform === 'win32' || !child.pid || child.exitCode !== null || child.signalCode !== null) return;
    try { process.kill(-child.pid, 'SIGTERM'); }
    catch (error) { if (error.code !== 'ESRCH') throw error; }
  };
  const trackedSpawn = (...args) => {
    const child = originalSpawn(...args);
    if (trackingStartup && args[2]?.detached === true) {
      startupChildren.add(child);
      child.once('exit', (code, signal) => {
        // The pinned runner polls exitCode but does not inspect signalCode.
        // Node keeps exitCode null after SIGTERM. A confirmed cancelled exit
        // uses its conventional nonzero signal status so that loop terminates.
        if (ending && code === null && signal) child.exitCode = 128 + (os.constants.signals[signal] ?? 1);
      });
      // Port allocation is asynchronous. EOF can arrive before spawn returns;
      // cancel that newly acquired child as well, then let upstream clean up.
      if (ending) cancelStartupChild(child);
    }
    return child;
  };
  // This helper is a dedicated process. Tracking its startup handles adds host
  // cancellation without changing any vendored source, argv or environment.
  childProcess.spawn = trackedSpawn;
  syncBuiltinESMExports();
  const stop = () => {
    ending = true;
    stopping ??= (async () => {
      for (const child of startupChildren) cancelStartupChild(child);
      // Upstream owns bounded startup and cleanup. An EOF while starting must
      // still retire the resulting server before this service exits.
      try { await starting; } catch { /* Startup reports its own diagnostic. */ }
      if (preview) await preview.stop();
      input.pause();
      input.off('data', onData);
      input.off('end', onEnd);
      input.off('error', onInputError);
      process.off('SIGTERM', onSignal);
      process.off('SIGINT', onSignal);
      if (childProcess.spawn === trackedSpawn) {
        childProcess.spawn = originalSpawn;
        syncBuiltinESMExports();
      }
      resolveDone();
    })().catch((error) => { report(error); resolveDone(); });
    return stopping;
  };
  const begin = async (line) => {
    const request = JSON.parse(line.toString('utf8'));
    if (!request || typeof request !== 'object' || Array.isArray(request)
      || typeof request.workspace !== 'string' || !request.workspace.trim())
      throw new RuntimeError('INVALID_INPUT', 'Expected {workspace,plan}');
    checkPackage({ expectedManifestSha256: request.expectedManifestSha256 });
    const [{ parsePreviewPlan }, { startDesignPreview }] = await Promise.all([
      import(pathToFileURL(path.join(runtimeRoot, 'vendor/tastecode-design/dist/index.js')).href),
      import(pathToFileURL(path.join(runtimeRoot, 'vendor/tastecode-design/server/dist/preview.js')).href),
    ]);
    const workspace = realpathSync(request.workspace);
    const plan = parsePreviewPlan(request.plan);
    if (ending) return;
    trackingStartup = true;
    try { preview = await startDesignPreview(workspace, plan); }
    finally { trackingStartup = false; }
    if (!ending) emit({ ready: true, url: preview.url, viewports: preview.viewports });
  };
  function onData(chunk) {
    if (ending) return;
    if (started) {
      if (chunk.toString('utf8').trim()) {
        report(new RuntimeError('INVALID_INPUT', 'A preview service accepts only one start request; close stdin to stop it'));
        void stop();
      }
      return;
    }
    buffer = Buffer.concat([buffer, Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk)]);
    if (buffer.length > MAX_WIRE_BYTES) {
      report(new RuntimeError('INPUT_TOO_LARGE', 'Preview request exceeds the 8 MiB transport limit'));
      void stop();
      return;
    }
    const newline = buffer.indexOf(10);
    if (newline < 0) return;
    const line = buffer.subarray(0, newline);
    if (buffer.subarray(newline + 1).toString('utf8').trim()) {
      report(new RuntimeError('INVALID_INPUT', 'A preview service accepts only one start request'));
      void stop();
      return;
    }
    buffer = Buffer.alloc(0);
    started = true;
    starting = begin(line).catch((error) => { if (!ending) report(error); });
    void starting.then(() => { if (errorSent || ending) void stop(); });
  }
  function onEnd() {
    if (!started && buffer.toString('utf8').trim()) report(new RuntimeError('INVALID_INPUT', 'Preview start request must end with a newline; keep stdin open until stopping'));
    void stop();
  }
  function onInputError(error) { report(error); void stop(); }
  function onSignal() { void stop(); }
  input.on('data', onData);
  input.once('end', onEnd);
  input.once('error', onInputError);
  process.on('SIGTERM', onSignal);
  process.on('SIGINT', onSignal);
  output.on('error', () => { void stop(); });
  input.resume();
  await done;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await previewService();
