//! Live browser view — the current coworker's page, streamed into the app.
//!
//! The managed chrome stays headless (user spec 2026-07-12: "no headless not
//! on desktop — a special browser window on the canvas opens and I see the
//! browser in the canvas"). CDP `Page.startScreencast` pushes damage-driven
//! JPEG frames; each armed tab acks + fans them out through a process-global
//! broadcast channel that `SubscribeBrowser` daemon connections tap. Frames
//! NEVER touch the postbox or the story lane — those replay and persist, and
//! a 10fps JPEG stream would bloat both. No subscribers = frames are acked
//! and dropped on the floor (chrome only streams while the page changes).

use super::*;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// One screencast frame from a live tab, ready for a UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserFrame {
    /// Runtime browser instance/profile scope. Empty is the canonical profile.
    pub instance: String,
    /// The tab's URL at frame time (cached CDP value — cheap).
    pub url: String,
    /// Base64 JPEG.
    /// Shared because Tokio broadcast clones one frame per Canvas subscriber.
    /// An Arc keeps the large base64 JPEG from being copied at every hop; its
    /// JSON wire representation remains the same string.
    pub data: Arc<str>,
    pub w: u32,
    pub h: u32,
}

fn frames() -> &'static broadcast::Sender<BrowserFrame> {
    static CHANNEL: std::sync::OnceLock<broadcast::Sender<BrowserFrame>> =
        std::sync::OnceLock::new();
    // Tiny ring on purpose: a slow client skips to the newest frame
    // (broadcast lags drop the oldest), it never builds a backlog.
    CHANNEL.get_or_init(|| broadcast::channel(3).0)
}

/// Subscribe a UI to the live frame stream (daemon `SubscribeBrowser`).
pub fn frames_subscribe() -> broadcast::Receiver<BrowserFrame> {
    frames().subscribe()
}

fn generations() -> &'static Mutex<std::collections::HashMap<String, u64>> {
    static GENERATIONS: std::sync::OnceLock<Mutex<std::collections::HashMap<String, u64>>> =
        std::sync::OnceLock::new();
    GENERATIONS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn next_generation(instance: &str) -> u64 {
    let mut values = generations()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let generation = values.entry(instance.to_string()).or_default();
    *generation = generation.wrapping_add(1).max(1);
    *generation
}

fn generation_is_current(instance: &str, generation: u64) -> bool {
    generations()
        .lock()
        .map(|values| values.get(instance).copied() == Some(generation))
        .unwrap_or(false)
}

/// Stop JPEG production while the compositor displays Chromium's real window.
/// Advancing the generation retires every prior CDP event listener as well as
/// stopping the producer, so a later fallback `arm` never publishes duplicate
/// frames through old listeners retained by the Tab.
pub(super) fn disarm(tab: &Arc<Tab>, instance: &str) {
    next_generation(instance);
    if let Err(error) = tab.stop_screencast() {
        tracing::debug!("browser view: screencast stop was unnecessary or failed: {error:#}");
    }
}

/// Arm the screencast on a working tab. Every path that swaps the working tab
/// (launch, attach, reconnect, `adopt_tab`) must re-arm — a listener holds a
/// Weak of THIS tab only. Failures degrade silently: the view is a luxury,
/// browsing must never break because streaming could not start.
pub(super) fn arm(tab: &Arc<Tab>, instance: &str) {
    use headless_chrome::protocol::cdp::types::Event;
    use headless_chrome::protocol::cdp::Page::StartScreencastFormatOption;
    use std::sync::atomic::{AtomicU64, Ordering};
    if super::native_surface_attached(instance) {
        disarm(tab, instance);
        super::blog(&format!(
            "screencast: disarmed for native surface on instance '{instance}'"
        ));
        return;
    }
    let generation = next_generation(instance);
    let weak = Arc::downgrade(tab);
    let instance = instance.to_string();
    let listener_instance = instance.clone(); // the closure owns its own copy; `instance` stays for the outer blog lines
                                              // DIAGNOSTIC (2026-07-15, "browser opens headless instead of in the canvas"):
                                              // the pipeline degrades silently, so make frame flow VISIBLE in gateway.log.
                                              // frames_seen: total frames chrome pushed; no_sub_logged: one-shot so a
                                              // "no canvas subscriber" drop is reported once, not per frame.
    let frames_seen = Arc::new(AtomicU64::new(0));
    let no_sub_logged = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let listener = Arc::new(move |event: &Event| {
        if let Event::PageScreencastFrame(frame) = event {
            if !generation_is_current(&listener_instance, generation)
                || super::native_surface_attached(&listener_instance)
            {
                return;
            }
            let Some(tab) = weak.upgrade() else { return };
            // Ack immediately and unconditionally — chrome stops streaming
            // after a few unacked frames, subscribers or not.
            let _ = tab.ack_screencast(frame.params.session_id);
            let sender = frames();
            if sender.receiver_count() == 0 {
                // Reveal the race/regression: chrome IS streaming but no canvas
                // is subscribed, so every frame is dropped on the floor.
                if !no_sub_logged.swap(true, Ordering::Relaxed) {
                    super::blog(&format!(
                        "screencast: frames arriving for instance '{listener_instance}' but NO canvas subscriber — dropping (view will look dead)"
                    ));
                }
                return; // nobody watching — drop the frame
            }
            let n = frames_seen.fetch_add(1, Ordering::Relaxed);
            if n == 0 || n % 200 == 0 {
                super::blog(&format!(
                    "screencast: frame {} streamed to canvas (instance '{listener_instance}', {}x{})",
                    n + 1,
                    frame.params.metadata.device_width as u32,
                    frame.params.metadata.device_height as u32
                ));
            }
            let meta = &frame.params.metadata;
            let _ = sender.send(BrowserFrame {
                instance: listener_instance.clone(),
                url: tab.get_url(),
                data: Arc::from(frame.params.data.as_str()),
                w: meta.device_width as u32,
                h: meta.device_height as u32,
            });
        }
    });
    if let Err(error) = tab.add_event_listener(listener) {
        tracing::warn!("browser view: screencast listener not attached: {error:#}");
        super::blog(&format!(
            "screencast: listener NOT attached ({error:#}) — no in-canvas view"
        ));
        return;
    }
    // The viewport follows the actual in-app browser size. Binary WebSocket
    // delivery and newest-frame decode backpressure let us preserve full
    // desktop resolution without building a base64 queue in WebKit.
    if let Err(error) = tab.start_screencast(
        Some(StartScreencastFormatOption::Jpeg),
        Some(74),   // fallback only: readable text without saturating CPU
        Some(2560), // max width — matches the safe interactive viewport cap
        Some(1600), // max height
        // This is a compatibility fallback when native X11 embedding is not
        // available. Skip alternate producer frames to bound JPEG encode +
        // WebKit decode CPU; native mode paints compositor pixels directly.
        Some(2), // ~30fps fallback; the native compositor path owns full-rate input/rendering
    ) {
        tracing::warn!("browser view: screencast did not start: {error:#}");
        super::blog(&format!(
            "screencast: did NOT start on instance '{instance}' ({error:#}) — no in-canvas view"
        ));
        return;
    }
    super::blog(&format!(
        "screencast: armed + started on instance '{instance}' (streaming into canvas)"
    ));
}
