"use strict";

// The backend and private browsers outlive the interface. Their notifications
// must not target a crashed or navigating frame; the new UI restores durable
// history and browser status when it loads. Never replay an old native event.
function send(contents, name, payload) {
  try {
    if (!contents || contents.isDestroyed() || contents.isCrashed()
        || contents.isLoadingMainFrame() || contents.getOSProcessId() <= 0) return false;
    contents.send("phoenix:event", name, payload);
    return true;
  } catch (error) {
    // Disposal can race the health checks above.
    if (/render frame.*disposed|webframe.*(?:disposed|destroyed)|object has been destroyed/i
        .test(String(error?.message || error))) return false;
    throw error;
  }
}

module.exports = { send };
