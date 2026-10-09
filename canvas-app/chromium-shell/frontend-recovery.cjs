"use strict";

// Window controls in a stalled renderer cannot recover that renderer. Keep
// this failure control in the native main process, independent of chat/jobs.
const RECOVERY = Symbol.for("phoenix.frontend.recovery");

function install(window, {
  frontendURL, dialog, getAllWebContents, log = () => {},
  setTimer = setTimeout, clearTimer = clearTimeout, delay = 5000,
}) {
  const contents = window.webContents;
  if (contents[RECOVERY]) return contents[RECOVERY];
  if (!frontendURL) throw new Error("An exact frontend URL is required");
  let closed = false, stalled = false, prompted = false, recovering = false;
  let generation = 0, timer = null, question = null, lastError = null;
  let remapAfterLoad = false;
  let lastRendererPid = contents.getOSProcessId(), lastFailure = null;

  function clearPending() {
    generation++;
    if (timer !== null) clearTimer(timer);
    timer = null;
    question?.abort();
    question = null;
  }

  async function offerRecovery(token) {
    timer = null;
    if (closed || !stalled || prompted || recovering || token !== generation
        || window.isDestroyed() || contents.isDestroyed()) return;
    prompted = true;
    const controller = question = new AbortController();
    try {
      const choice = await dialog.showMessageBox(window, {
        type: "warning", title: "Phoenix interface",
        message: "The Phoenix interface stopped responding.",
        detail: "Reload the interface to continue. The gateway and browser tabs stay running. Unsaved edits may be lost.",
        buttons: ["Reload interface", "Keep waiting"],
        defaultId: 1, cancelId: 1, noLink: true, signal: controller.signal,
      });
      if (choice.response !== 0 || closed || !stalled || token !== generation
          || window.isDestroyed() || contents.isDestroyed()) return;
      const currentURL = contents.getURL();
      if (![frontendURL, "", "chrome-error://chromewebdata/"].includes(currentURL)) {
        throw new Error("Recovery refused: the native window changed its frontend");
      }
      const pid = contents.getOSProcessId();
      if (pid > 0 && getAllWebContents().some(other => other !== contents
          && !other.isDestroyed() && other.getOSProcessId() === pid)) {
        throw new Error("Recovery refused: another native view shares this renderer");
      }
      recovering = true;
      remapAfterLoad = true;
      log("reloading the interface renderer " + pid + "; gateway and browser views retained");
      if (!contents.isCrashed()) contents.forcefullyCrashRenderer();
      contents.reload();
    } catch (error) {
      if (!controller.signal.aborted) {
        lastError = String(error?.message || error);
        recovering = false;
        remapAfterLoad = false;
        log("interface recovery: " + lastError);
      }
    } finally {
      if (question === controller) question = null;
    }
  }

  function onStalled() {
    if (closed || recovering || prompted || timer !== null) return;
    stalled = true;
    lastError = null;
    const token = generation;
    log("interface renderer " + contents.getOSProcessId() + " is unresponsive");
    timer = setTimer(() => void offerRecovery(token), delay);
    timer?.unref?.();
  }
  function onResponsive() {
    clearPending();
    stalled = false;
    prompted = false;
    recovering = false;
  }
  function onLoaded() {
    lastRendererPid = contents.getOSProcessId();
    // On Wayland a replaced renderer can have a healthy DOM but receive no
    // animation frames until the existing native surface is mapped again.
    // Retain the window and all child views; only refresh its mapping after
    // a recovery the user chose. Preserve hidden/minimized window state.
    if (remapAfterLoad && !closed && !window.isDestroyed()) {
      remapAfterLoad = false;
      if (window.isVisible() && !window.isMinimized()) {
        const focused = window.isFocused();
        window.hide();
        if (focused) window.show();
        else window.showInactive();
        log("interface drawing surface reattached; native browser views retained");
      }
    }
    onResponsive();
  }
  function onInput(event, input) {
    if (!stalled || question || closed || recovering || input.type !== "keyDown"
        || !(input.control || input.meta) || !input.shift || input.alt
        || String(input.key).toLowerCase() !== "r") return;
    event.preventDefault();
    clearPending();
    prompted = false;
    void offerRecovery(generation);
  }
  function onGone(_event, details) {
    lastFailure = { reason:details?.reason || "unknown", exitCode:details?.exitCode ?? null,
      pid:contents.getOSProcessId() || lastRendererPid, at:new Date().toISOString() };
    log("interface renderer ended " + JSON.stringify(lastFailure));
    if (!recovering) onStalled();
  }
  function dispose() {
    if (closed) return;
    closed = true;
    clearPending();
    contents.removeListener("unresponsive", onStalled);
    contents.removeListener("responsive", onResponsive);
    contents.removeListener("did-finish-load", onLoaded);
    contents.removeListener("render-process-gone", onGone);
    contents.removeListener("before-input-event", onInput);
    window.removeListener("closed", dispose);
    delete contents[RECOVERY];
  }
  const control = Object.freeze({
    status: () => ({ stalled, prompted, recovering, lastError,
      lastFailure:lastFailure ? {...lastFailure} : null }), dispose,
  });
  contents[RECOVERY] = control;
  contents.on("unresponsive", onStalled);
  contents.on("responsive", onResponsive);
  contents.on("did-finish-load", onLoaded);
  contents.on("render-process-gone", onGone);
  contents.on("before-input-event", onInput);
  window.on("closed", dispose);
  return control;
}

module.exports = { install };
