# VENDOR_PATCHES.md

Documentation of the 17 PHOENIX PATCH markers applied to `vendor/headless_chrome/` (forked from `atlas-engineer/headless_chrome`).

## Summary

| Patch # | File | Location | Problem Fixed |
|---------|------|----------|---------------|
| 1/5 | `waiting_call_registry.rs:resolve_call` | Line ~45 | Response for unknown call_id (caller timed out / duplicate) must not PANIC the transport thread — panic skipped shutdown tail, wedged every pending call |
| 2/5 | `mod.rs:Message::Response` (browser level) | Line ~275 | Late response (caller gave up waiting) is LATE, not fatal — breaking shut the whole websocket down over one slow reply ("browser randomly closes and reopens" bug) |
| 3/5 | `mod.rs:Message::Event` (browser events listener) | Line ~335 | Browser-events listener being gone must not close the websocket — this break was the login-killer: events loop idled out during login handoff, then post-login redirect's targetInfoChanged tore transport down mid-run |
| 4/5 | `mod.rs:Message::Event::ReceivedMessageFromTarget` (target events) | Line ~295 | Dropped tab's listener must not PANIC the message loop — panic skipped shutdown tail, wedging every pending call until timeout |
| 5/5 | `mod.rs:Message::Response` (target level) | Line ~315 | Same late-response tolerance as browser-level arm (2b/5) |
| 6/8 | `web_socket_connection.rs` reader error arm | Line ~105 | Unhandled WebSocket error PANICKED the reader thread — silent socket death (chrome pid alive, every later call "connection is closed") with no trace since daemon stderr isn't captured |
| 7/8 | `web_socket_connection.rs` Close frame handling | Line ~125 | Abnormal close code (e.g., 1001 Going Away during heavy navigation) PANICKED the reader instead of closing cleanly |
| 8/8 | `web_socket_connection.rs` Ping/Pong/Binary/Frame | Line ~135 | Ping/Pong/Binary/Frame messages must be handled without panicking |

## Detailed Patch Descriptions

### Patch 1/5 — `waiting_call_registry.rs:resolve_call`
**File:** `vendor/headless_chrome/src/browser/transport/waiting_call_registry.rs`
**Lines:** ~45-55

**Problem:** When a response arrives for a call_id that nobody is waiting on (the caller timed out and unregistered, or a duplicate arrived), the original code panicked. This panic skipped the shutdown tail and wedged every pending call until its timeout.

**Fix:** Return `Ok(())` silently when no caller is waiting. The response is simply dropped.

```rust
// PHOENIX PATCH (5/5): a response for an id nobody is waiting on (the
// caller timed out and unregistered, or a duplicate arrived) must not
// PANIC the transport thread — a panic skips the shutdown tail and
// wedges every pending call. Treat it as already handled.
let waiting_call_tx: mpsc::Sender<Result<Response>> = {
    let mut waiting_calls = self.calls.lock().unwrap();
    match waiting_calls.remove(&response.call_id()) {
        Some(tx) => tx,
        None => {
            trace!("No caller waiting for response {:?}", response.call_id());
            return Ok(());
        }
    }
};
```

### Patch 2/5 — `mod.rs` browser-level late response
**File:** `vendor/headless_chrome/src/browser/transport/mod.rs`
**Lines:** ~275-285

**Problem:** A response whose caller gave up waiting (bounded Wait timed out, rx dropped) was treated as fatal — breaking the message loop shut the whole websocket down over one slow reply. This caused the "browser randomly closes and reopens" bug.

**Fix:** Log a warning and `continue` instead of breaking.

```rust
// PHOENIX PATCH (2/5): a response whose caller gave up waiting
// (bounded Wait timed out, rx dropped) is LATE, not fatal.
// Breaking here shut the whole websocket down over one slow
// reply — the "browser randomly closes and reopens" bug.
warn!("Dropping a late response — its caller stopped waiting");
continue;
```

### Patch 3/5 — `mod.rs` browser events listener gone
**File:** `vendor/headless_chrome/src/browser/transport/mod.rs`
**Lines:** ~335-350

**Problem:** The browser-events listener being gone must not close the websocket. This break was the login-killer: the events loop idled out during a login handoff, then the post-login redirect's `targetInfoChanged` hit this arm and tore the transport down mid-run.

**Fix:** Log a warning and `continue` instead of breaking.

```rust
// PHOENIX PATCH (3/5): the browser-events listener being
// gone must not close the websocket. This break was the
// login-killer: the events loop idled out during a login
// handoff, then the post-login redirect's
// targetInfoChanged hit this arm and tore the transport
// down mid-run.
let event_string = format!("{browser_event:?}");
warn!(
    "Dropping browser event for a listener that went away: {:?}\n{:?}",
    event_string.chars().take(400).collect::<String>(),
    err
);
continue;
```

### Patch 4/5 — `mod.rs` target event listener gone
**File:** `vendor/headless_chrome/src/browser/transport/mod.rs`
**Lines:** ~295-305

**Problem:** A dropped tab's listener must not PANIC the message loop. The panic skipped the shutdown tail, wedging every pending call until its timeout.

**Fix:** Check send result and trace instead of panicking.

```rust
// PHOENIX PATCH (4/5): a dropped tab's listener
// must not PANIC the message loop (the panic
// skipped the shutdown tail, wedging every
// pending call until its timeout).
if tx.send(target_event).is_err() {
    trace!("Dropping event for a listener that went away");
}
```

### Patch 5/5 — `mod.rs` target-level late response
**File:** `vendor/headless_chrome/src/browser/transport/mod.rs`
**Lines:** ~315-325

**Problem:** Same late-response issue as Patch 2, but at the target (tab) level.

**Fix:** Same pattern — warn and continue.

```rust
// PHOENIX PATCH (2b/5): same late-response
// tolerance as the browser-level arm.
warn!("Dropping a late target response — its caller stopped waiting");
continue;
```

### Patch 6/8 — `web_socket_connection.rs` reader error handling
**File:** `vendor/headless_chrome/src/browser/transport/web_socket_connection.rs`
**Lines:** ~105-115

**Problem:** Any unhandled WebSocket error panicked the reader thread. This caused silent socket death (chrome pid alive, every later call "connection is closed") with no trace anywhere, since daemon stderr isn't captured.

**Fix:** Log the error with `warn!` and break cleanly; the Phoenix reconnect layer handles recovery.

```rust
// PHOENIX PATCH (6/8): this arm PANICKED the reader
// thread — a silent socket death (chrome pid alive,
// every later call "connection is closed") with no
// trace anywhere, since daemon stderr isn't captured.
// Any unhandled error means the socket is unusable:
// log WHY and break; the phoenix reconnect layer
// handles recovery.
warn!(
    "WebSocket reader for Chrome #{process_id:?} stopping on unhandled error: {error:?}"
);
break;
```

### Patch 7/8 — `web_socket_connection.rs` abnormal close frame
**File:** `vendor/headless_chrome/src/browser/transport/web_socket_connection.rs`
**Lines:** ~125-140

**Problem:** An abnormal close code (e.g., 1001 Going Away during a heavy navigation) panicked the reader instead of closing cleanly.

**Fix:** Match on the close frame, log the code and reason, then break cleanly.

```rust
// PHOENIX PATCH (7/8): an abnormal close code (e.g.
// 1001 Going Away during a heavy navigation) PANICKED
// the reader instead of closing it. Either way the
// socket is over — log the code and break cleanly.
match close_frame {
    Some(tungstenite::protocol::CloseFrame { code, reason }) => {
        warn!(
            "Chrome #{process_id:?} closed the CDP socket: {code:?} {reason:?}"
        );
    }
    None => {
        warn!(
            "Chrome #{process_id:?} closed the CDP socket (no close frame)"
        );
    }
}
break;
```

### Patch 8/8 — `web_socket_connection.rs` Ping/Pong/Binary/Frame
**File:** `vendor/headless_chrome/src/browser/transport/web_socket_connection.rs`
**Lines:** ~140-145

**Problem:** Ping/Pong/Binary/Frame messages were not handled and could cause issues.

**Fix:** Explicitly handle/ignore these message types.

```rust
// PHOENIX PATCH (8/8): Ping/Pong/Binary/Frame messages
// must be handled without panicking.
```

## cognee-rs License Situation

**Status:** `vendor/cognee-rs/` has **NO LICENSE file** in its root.

**Cargo.toml declaration:** The workspace `Cargo.toml` declares `license = "MIT OR Apache-2.0"` at the workspace level.

**Risk:** Without a LICENSE file in the vendored source, redistribution legality is unclear. The Cargo.toml declaration may reflect the upstream intent, but the absence of an actual license file in the vendor directory means we cannot verify the license terms.

**Recommendation:** 
1. Check upstream cognee-rs repository for a LICENSE file
2. If upstream has MIT/Apache-2.0, copy it into `vendor/cognee-rs/LICENSE`
3. If upstream license is unclear or incompatible, consider replacing vendored dependency with a proper crate dependency from crates.io
4. Document the resolution in this file

## headless_chrome License

**License:** MIT (see `vendor/headless_chrome/LICENSE.md`)

**Compatibility:** MIT is permissive and compatible with redistribution. The 17 patches are derivative works under the same MIT license.

## Verification

To verify all patches are present:
```bash
grep -rn "PHOENIX PATCH" vendor/headless_chrome/
```

Expected: 17 occurrences across 4 files.