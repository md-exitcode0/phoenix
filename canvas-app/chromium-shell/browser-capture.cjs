"use strict";

// A hidden, parented WebContentsView needs frames to satisfy CDP capture.
// Own a temporary debugger session instead of running a permanent video feed.
const active = new WeakSet();
const MAX_CAPTURE_BYTES = 24 * 1024 * 1024;
const MAX_CAPTURE_PIXELS = 32 * 1024 * 1024;

function deadline(promise, milliseconds, message) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(message)), milliseconds); }),
  ]).finally(() => clearTimeout(timer));
}

// `lease` lends a parked tab a drawable spot for the capture only: on Wayland a
// never-shown window produces no frames, so parked tabs could not be captured.
// `viewport` keeps the page laid out at its real size while it borrows it.
async function captureScreenshot(contents, { fullPage = false, timeoutMs = 8000, inputReady = false, clipOverride = null, lease = null, viewport = null } = {}) {
  if (contents.isDestroyed()) throw new Error("Browser tab is no longer open");
  if (clipOverride !== null && (!fullPage || inputReady || typeof clipOverride !== "object"
      || clipOverride.x !== 0 || clipOverride.y !== 0 || clipOverride.scale !== 1
      || !Number.isSafeInteger(clipOverride.width) || !Number.isSafeInteger(clipOverride.height)
      || clipOverride.width < 320 || clipOverride.width > 3840
      || clipOverride.height < 240 || clipOverride.height > 12000
      || clipOverride.width * clipOverride.height > MAX_CAPTURE_PIXELS)) {
    throw new Error("Design preview capture has invalid dimensions");
  }
  if (active.has(contents) || contents.debugger.isAttached()) {
    throw new Error("Browser tab already has an active capture or debugger");
  }
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) throw new Error("Invalid browser capture deadline");
  active.add(contents);
  const inspector = contents.debugger;
  const expires = Date.now() + timeoutMs;
  let attached = false, pumping = false, primaryError, release = null, viewportOverridden = false;
  const send = (method, params = {}) => {
    const remaining = expires - Date.now();
    if (remaining <= 0) throw new Error("Browser screenshot timed out");
    return deadline(inspector.sendCommand(method, params), remaining, "Browser screenshot timed out");
  };
  const acknowledge = (_event, method, params) => {
    if (pumping && method === "Page.screencastFrame") {
      inspector.sendCommand("Page.screencastFrameAck", { sessionId: params.sessionId }).catch(() => {});
    }
  };
  try {
    inspector.attach("1.3");
    attached = true;
    inspector.on("message", acknowledge);
    await send("Page.enable");
    // Exact design clips already have a caller-owned emulated viewport.
    if (viewport && !clipOverride) {
      viewportOverridden = true;
      await send("Emulation.setDeviceMetricsOverride", {
        width: viewport.width, height: viewport.height, deviceScaleFactor: 0, mobile: false,
      });
    }
    if (lease) release = lease();
    let clip;
    if (inputReady) {
      const { cssVisualViewport: viewport } = await send("Page.getLayoutMetrics");
      if (!viewport || !Number.isFinite(viewport.pageX) || !Number.isFinite(viewport.pageY)
          || !Number.isFinite(viewport.clientWidth) || !Number.isFinite(viewport.clientHeight)
          || viewport.clientWidth < 1 || viewport.clientHeight < 1) throw new Error("Browser input viewport is unavailable");
      clip = { x: viewport.pageX, y: viewport.pageY, width: 1, height: 1, scale: 1 };
    }
    if (clipOverride) {
      clip = { ...clipOverride };
    } else if (fullPage) {
      const { cssContentSize } = await send("Page.getLayoutMetrics");
      const width = Math.ceil(cssContentSize?.width), height = Math.ceil(cssContentSize?.height);
      if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0
          || width > 16384 || height > 16384 || width * height > MAX_CAPTURE_PIXELS) {
        throw new Error("Full-page screenshot exceeds the supported image dimensions");
      }
      clip = { x: 0, y: 0, width, height, scale: 1 };
    }
    pumping = true;
    await send("Page.startScreencast", { format: "png", maxWidth: inputReady ? 1 : 800, maxHeight: inputReady ? 1 : 600, everyNthFrame: 1 });
    // Waiting for a screencast event before this request can deadlock a cold
    // hidden view. Capture itself requests the first compositor frame.
    const result = await send("Page.captureScreenshot", {
      format: "png", fromSurface: true, captureBeyondViewport: fullPage,
      ...(clip ? { clip } : {}),
    });
    const maxBytes = inputReady ? 4096 : MAX_CAPTURE_BYTES;
    if (typeof result?.data !== "string" || result.data.length > Math.ceil(maxBytes / 3) * 4) {
      throw new Error("Browser returned an invalid or oversized screenshot");
    }
    const bytes = Buffer.from(result.data, "base64");
    if (bytes.length > maxBytes || !bytes.subarray(0, 8).equals(Buffer.from([137,80,78,71,13,10,26,10]))) {
      throw new Error("Browser returned no valid PNG screenshot");
    }
    // One CSS pixel can occupy several device pixels on a scaled display.
    if (inputReady && (bytes.length < 33 || bytes.toString("ascii", 12, 16) !== "IHDR"
        || bytes.readUInt32BE(16) < 1 || bytes.readUInt32BE(16) > 16
        || bytes.readUInt32BE(20) < 1 || bytes.readUInt32BE(20) > 16)) {
      throw new Error("Browser input readiness returned an invalid frame");
    }
    return bytes;
  } catch (error) {
    primaryError = error;
    throw error;
  } finally {
    pumping = false;
    let cleanupError;
    try { release?.(); } catch (error) { cleanupError = error; }
    try {
      inspector.removeListener("message", acknowledge);
      if (attached && !contents.isDestroyed() && inspector.isAttached()) {
        try {
          await deadline(inspector.sendCommand("Page.stopScreencast"), 750, "Browser capture cleanup timed out");
        } finally {
          try {
            if (viewportOverridden && !contents.isDestroyed() && inspector.isAttached()) {
              await deadline(inspector.sendCommand("Emulation.clearDeviceMetricsOverride"), 750, "Browser viewport cleanup timed out");
            }
          } finally {
            // Detaching also retires a late start/capture after a deadline.
            if (!contents.isDestroyed() && inspector.isAttached()) inspector.detach();
          }
        }
      }
    } catch (error) { cleanupError = error; }
    finally { active.delete(contents); }
    if (cleanupError && !primaryError) {
      throw new Error("Browser capture cleanup failed", { cause: cleanupError });
    }
  }
}

// A cold parked view acknowledges CDP mouse presses without delivering them.
// A tiny compositor frame prepares that exact view before any gesture. This is
// not visual evidence, performs no input, and is never cached across navigation.
async function prepareInput(contents, lease = {}) {
  await captureScreenshot(contents, { inputReady: true, timeoutMs: 5000, ...lease });
}

// A page previously resized by a streamed/captured lane must follow the real
// WebContentsView again when it is mounted or resized. Never steal a debugger.
async function restoreViewport(contents) {
  if (contents.isDestroyed() || active.has(contents) || contents.debugger.isAttached()) return false;
  active.add(contents);
  const inspector = contents.debugger;
  let attached = false;
  try {
    inspector.attach("1.3");attached = true;
    await deadline(inspector.sendCommand("Emulation.clearDeviceMetricsOverride"), 750, "Browser viewport reset timed out");
    return true;
  } finally {
    if (attached && !contents.isDestroyed() && inspector.isAttached()) inspector.detach();
    active.delete(contents);
  }
}
module.exports = { captureScreenshot, prepareInput, restoreViewport };
