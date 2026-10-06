// Phoenix Agent Cursor — GNOME Shell extension.
//
// The compositor half of Phoenix's computer_use agent on GNOME Wayland:
//  - draws a separate, high-contrast animated agent cursor (the user's
//    real pointer is never hijacked; it is saved/restored around actions)
//  - injects input through Clutter virtual devices (pointer + keyboard)
//  - takes screenshots through Shell's own screenshot service
//  - exports everything on the session bus as dev.phoenix.Cursor
//
// Phoenix calls this over `gdbus` from src/tools/computer_use.rs.

import Cairo from 'cairo';
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import {paintCursor, movementDuration} from './cursor-art.mjs';
import {validateWindowActions, parseKeyCombo, keypadKeycode, KEYSYMS} from './cursor-input.mjs';

const BUS_NAME = 'dev.phoenix.Cursor';
const OBJECT_PATH = '/dev/phoenix/Cursor';

// Where the arrow tip sits inside the cursor widget (px from top-left). Offset
// inward so the contrast outline does not clip at the actor boundary.
const CURSOR_HOTSPOT = 10;

const IFACE_XML = `
<node>
  <interface name="dev.phoenix.Cursor">
    <method name="Status">
      <arg type="s" direction="out" name="json"/>
    </method>
    <method name="MoveTo">
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
      <arg type="u" direction="in" name="duration_ms"/>
    </method>
    <method name="Click">
      <arg type="s" direction="in" name="button"/>
    </method>
    <method name="DoubleClick">
      <arg type="s" direction="in" name="button"/>
    </method>
    <method name="Drag">
      <arg type="i" direction="in" name="to_x"/>
      <arg type="i" direction="in" name="to_y"/>
      <arg type="u" direction="in" name="duration_ms"/>
    </method>
    <method name="Scroll">
      <arg type="i" direction="in" name="dx"/>
      <arg type="i" direction="in" name="dy"/>
    </method>
    <method name="TypeText">
      <arg type="s" direction="in" name="text"/>
    </method>
    <method name="Key">
      <arg type="s" direction="in" name="combo"/>
    </method>
    <method name="Screenshot">
      <arg type="s" direction="in" name="path"/>
      <arg type="s" direction="out" name="result"/>
    </method>
    <method name="SetRestorePointer">
      <arg type="b" direction="in" name="restore"/>
    </method>
    <method name="Hide"/>
    <method name="ListWindows">
      <arg type="s" direction="out" name="json"/>
    </method>
    <method name="FocusWindow">
      <arg type="u" direction="in" name="id"/>
      <arg type="s" direction="out" name="json"/>
    </method>
    <method name="CaptureWindow">
      <arg type="u" direction="in" name="id"/>
      <arg type="s" direction="in" name="path"/>
      <arg type="s" direction="out" name="json"/>
    </method>
    <method name="WindowBatch">
      <arg type="u" direction="in" name="id"/>
      <arg type="s" direction="in" name="actions_json"/>
      <arg type="s" direction="out" name="json"/>
    </method>
    <method name="LowerWindow">
      <arg type="u" direction="in" name="id"/>
      <arg type="s" direction="out" name="json"/>
    </method>
  </interface>
</node>`;

const BUTTONS = {
    left: Clutter.BUTTON_PRIMARY,
    right: Clutter.BUTTON_SECONDARY,
    middle: Clutter.BUTTON_MIDDLE,
};

export default class PhoenixCursorExtension extends Extension {
    enable() {
        const seat = Clutter.get_default_backend().get_default_seat();
        this._vpointer = seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
        this._vkeyboard = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
        this._restorePointer = true;
        this._ownedDesktop = /^desktop-[a-f0-9]{24}$/.test(GLib.getenv('PHOENIX_DESKTOP_SCOPE') || '');
        this._timeouts = new Set();

        this._buildCursor();

        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE_XML, this);
        this._dbus.export(Gio.DBus.session, OBJECT_PATH);
        this._nameId = Gio.bus_own_name(
            Gio.BusType.SESSION, BUS_NAME, Gio.BusNameOwnerFlags.NONE,
            null, () => this._publishOwnedDesktop(), null);
    }

    _publishOwnedDesktop() {
        // Only a compositor explicitly launched for a Phoenix scope publishes
        // a descriptor. Enabling the extension on the user's Shell never
        // exposes or changes that session's environment.
        const scope = GLib.getenv('PHOENIX_DESKTOP_SCOPE');
        if (!scope || !/^desktop-[a-f0-9]{24}$/.test(scope)) return;
        const runtime = GLib.getenv('XDG_RUNTIME_DIR');
        if (!runtime) throw new Error('Owned desktop has no runtime directory');
        const descriptor = {
            version: 1,
            scope_key: scope,
            runtime_dir: runtime,
            session_bus: GLib.getenv('DBUS_SESSION_BUS_ADDRESS'),
            wayland_display: GLib.getenv('WAYLAND_DISPLAY'),
            display: GLib.getenv('DISPLAY'),
            xauthority: GLib.getenv('XAUTHORITY'),
        };
        const file = Gio.File.new_for_path(GLib.build_filenamev([runtime, 'phoenix-desktop.json']));
        file.replace_contents(JSON.stringify(descriptor), null, false,
            Gio.FileCreateFlags.PRIVATE | Gio.FileCreateFlags.REPLACE_DESTINATION, null);
    }

    disable() {
        for (const shot of this._screenshots || []) shot.cancel('Cursor extension disabled');
        this._pointerAction?.finish('Cursor extension disabled');
        this._batch?.cancel('Cursor extension disabled');
        this._cancelMotion('Cursor extension disabled');
        for (const id of this._timeouts)
            GLib.source_remove(id);
        this._timeouts.clear();
        if (this._nameId) {
            Gio.bus_unown_name(this._nameId);
            this._nameId = 0;
        }
        if (this._dbus) {
            this._dbus.unexport();
            this._dbus = null;
        }
        this._cursor?.destroy();
        this._cursor = null;
        this._vpointer = null;
        this._vkeyboard = null;
    }

    _later(ms, fn) {
        const id = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
            this._timeouts.delete(id);
            fn();
            return GLib.SOURCE_REMOVE;
        });
        this._timeouts.add(id);
        return id;
    }

    // --- the agent cursor (overlay actor, never receives input) ------------

    _buildCursor() {
        // A crisp two-tone ember arrow. The hotspot is the arrow TIP
        // (top-left), not the actor's center or its decorative wing.
        this._cursor = new St.DrawingArea({
            reactive: false,
            can_focus: false,
            track_hover: false,
            width: 38,
            height: 48,
            opacity: 0,
            visible: true,
        });
        // The neon arrow art; the drawn vector arrow is only a fallback.
        this._cursorArt = null;
        try {
            const art = Cairo.ImageSurface.createFromPNG(`${this.path}/cursor.png`);
            if (art.getWidth() > 0 && art.getHeight() > 0) this._cursorArt = art;
        } catch (e) { console.warn(`phoenix-cursor: cursor art unavailable: ${e.message}`); }
        this._cursor.connect('repaint', area => this._paintArrow(area));
        Main.layoutManager.uiGroup.add_child(this._cursor);
        this._cursor.set_position(40, 40);
    }

    _paintArrow(area) {
        const cr = area.get_context();
        try { paintCursor(cr, CURSOR_HOTSPOT, this._cursorArt); }
        finally { cr.$dispose(); }
    }

    _cursorCenter() {
        // The action point is the arrow tip, not the widget center.
        return [
            Math.round(this._cursor.x + CURSOR_HOTSPOT),
            Math.round(this._cursor.y + CURSOR_HOTSPOT),
        ];
    }

    _showCursor() {
        this._cursor.remove_transition('opacity');
        this._cursor.ease({
            opacity: 255,
            duration: 150,
            mode: Clutter.AnimationMode.EASE_OUT_QUAD,
        });
        this._armIdleFade();
    }

    _armIdleFade() {
        if (this._idleFadeId) {
            GLib.source_remove(this._idleFadeId);
            this._timeouts.delete(this._idleFadeId);
        }
        this._idleFadeId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 8000, () => {
            this._timeouts.delete(this._idleFadeId);
            this._idleFadeId = 0;
            this._cursor?.ease({
                opacity: 0,
                duration: 600,
                mode: Clutter.AnimationMode.EASE_IN_QUAD,
            });
            return GLib.SOURCE_REMOVE;
        });
        this._timeouts.add(this._idleFadeId);
    }

    _spawnRipple(x, y, double) {
        const size = double ? 64 : 48;
        const ripple = new St.Widget({
            style_class: 'phoenix-cursor-ripple',
            reactive: false,
            width: size,
            height: size,
            opacity: 220,
        });
        Main.layoutManager.uiGroup.add_child(ripple);
        ripple.set_position(x - size / 2, y - size / 2);
        ripple.set_pivot_point(0.5, 0.5);
        ripple.set_scale(0.25, 0.25);
        ripple.ease({
            opacity: 0,
            scale_x: 1.0,
            scale_y: 1.0,
            duration: 350,
            mode: Clutter.AnimationMode.EASE_OUT_CUBIC,
            onComplete: () => ripple.destroy(),
        });
    }

    _requireMotionIdle() {
        if (this._motion || this._batch || this._pointerAction) throw new Error('Cursor busy: wait for the current input operation to finish');
    }

    _cancelMotion(reason) {
        const motion = this._motion;
        this._motion = null;
        if (!motion) return;
        this._clearMotionWatchdog(motion);
        try { this._cursor?.remove_all_transitions(); }
        finally { motion.cancel?.(reason); }
    }

    _clearMotionWatchdog(motion) {
        if (!motion.watchdog) return;
        GLib.source_remove(motion.watchdog);
        this._timeouts.delete(motion.watchdog);
        motion.watchdog = 0;
    }

    _animateTo(x, y, durationMs, onComplete, onCancel) {
        this._requireMotionIdle();
        this._showCursor();
        const targetX = x - CURSOR_HOTSPOT;
        const targetY = y - CURSOR_HOTSPOT;
        const dist = Math.hypot(targetX - this._cursor.x, targetY - this._cursor.y);
        // No per-move trail actors/timer. Keep automatic movement brief while
        // preserving an explicitly requested drag or demonstration duration.
        const duration = movementDuration(dist, durationMs);

        const motion = {cancel: onCancel};
        this._motion = motion;
        try {
        this._cursor.remove_all_transitions();
        this._cursor.opacity = 255;
        this._cursor.ease({
            x: targetX,
            y: targetY,
            duration,
            mode: Clutter.AnimationMode.EASE_IN_OUT_CUBIC,
            onComplete: () => {
                if (this._motion !== motion) return;
                this._motion = null;
                this._clearMotionWatchdog(motion);
                try { this._armIdleFade(); }
                finally { onComplete?.(); }
            },
        });
        // Compositor interruption does not always deliver onComplete. One
        // deadline per motion (not a polling loop) fences a missing callback.
        // Preserve explicit durations, including slow demonstration drags.
        if (this._motion === motion) {
            motion.watchdog = GLib.timeout_add(GLib.PRIORITY_DEFAULT,
                Math.min(0xffffffff, duration + 1000), () => {
                    this._timeouts.delete(motion.watchdog);
                    motion.watchdog = 0;
                    if (this._motion === motion)
                        this._cancelMotion('Cursor animation did not finish before its deadline');
                    return GLib.SOURCE_REMOVE;
                });
            this._timeouts.add(motion.watchdog);
        }
        } catch (error) {
            this._cancelMotion(error.message);
            if (!onCancel) throw error;
        }
    }

    // --- synthetic input (virtual devices; user pointer saved/restored) ----

    _now() {
        // Meta.Window.focus expects the compositor event clock in milliseconds.
        return global.get_current_time();
    }

    /// Put the logical pointer at (x, y). Virtual-device absolute motion alone
    /// is unreliable on some Mutter/Wayland versions — the following button
    /// press then lands wherever the USER's pointer happens to be. Warping the
    /// seat pointer first makes the position stick; the virtual motion is kept
    /// so apps still see a motion event.
    _pointerTo(x, y) {
        const seat = Clutter.get_default_backend().get_default_seat();
        if (seat.warp_pointer)
            seat.warp_pointer(x, y);
        this._vpointer?.notify_absolute_motion(GLib.get_monotonic_time(), x, y);
    }

    _withPointerAt(x, y, action, invocation) {
        const [userX, userY] = global.get_pointer();
        const restore = this._restorePointer;
        const lease = {timer: 0, finish: null};
        this._pointerAction = lease;
        lease.finish = reason => {
            if (this._pointerAction !== lease) return;
            if (lease.timer) {
                GLib.source_remove(lease.timer);
                this._timeouts.delete(lease.timer);
            }
            try { if (restore) this._pointerTo(userX, userY); }
            catch (error) { reason = reason || error.message; }
            this._pointerAction = null;
            if (reason) invocation.return_dbus_error('dev.phoenix.Cursor.Canceled', reason);
            else invocation.return_value(null);
        };
        let failure;
        try { this._pointerTo(x, y); action(); }
        catch (error) { failure = error.message; }
        if (restore) {
            // The wire reply marks the end of pointer ownership, not merely
            // button emission. No old restore can run inside a later action.
            try { lease.timer = this._later(120, () => {
                lease.timer = 0;
                lease.finish(failure);
            }); }
            catch (error) { lease.finish(failure || error.message); }
        } else {
            lease.finish(failure);
        }
    }

    _admitPointerAction(invocation) {
        if (this._motion || this._batch || this._pointerAction) {
            invocation.return_dbus_error('dev.phoenix.Cursor.Busy', 'Another input operation is still active');
            return false;
        }
        return true;
    }

    _pressRelease(button) {
        try {
            this._vpointer.notify_button(GLib.get_monotonic_time(), button, Clutter.ButtonState.PRESSED);
        } finally {
            this._vpointer.notify_button(GLib.get_monotonic_time(), button, Clutter.ButtonState.RELEASED);
        }
    }

    _keyvalTap(keyval) {
        const code=keypadKeycode(keyval);
        if(code!==null){
            try {this._vkeyboard.notify_key(GLib.get_monotonic_time(),code,Clutter.KeyState.PRESSED);}
            finally {this._vkeyboard.notify_key(GLib.get_monotonic_time(),code,Clutter.KeyState.RELEASED);}
            return;
        }
        this._withHeldKeys([keyval], () => {});
    }

    _withHeldKeys(keyvals, action) {
        const attempted = [];
        let failure;
        try {
            for (const keyval of keyvals) {
                // A native press may take effect before reporting failure.
                attempted.push(keyval);
                // Clutter virtual input uses monotonic microseconds, unlike
                // Meta.Window focus timestamps (Shell event milliseconds).
                this._vkeyboard.notify_keyval(GLib.get_monotonic_time(), keyval, Clutter.KeyState.PRESSED);
            }
            action();
        } catch (error) { failure = error; }
        finally {
            for (const keyval of attempted.reverse()) {
                try { this._vkeyboard.notify_keyval(GLib.get_monotonic_time(), keyval, Clutter.KeyState.RELEASED); }
                catch (error) { failure = failure || error; }
            }
        }
        if (failure) throw failure;
    }

    // --- DBus methods -------------------------------------------------------

    Status() {
        const [px, py] = global.get_pointer();
        const [ax, ay] = this._cursorCenter();
        // The coordinate space for clicks/moves is the FULL virtual stage in
        // LOGICAL pixels (origin top-left), NOT a single monitor. Reporting the
        // primary monitor (which can sit at a y-offset in a multi-monitor stack)
        // made the model reason against the wrong geometry and click in the
        // wrong place. Report the whole stage + per-monitor layout, origin (0,0).
        const stageW = global.screen_width;
        const stageH = global.screen_height;
        const monitors = Main.layoutManager.monitors.map(m => ({
            x: m.x, y: m.y, width: m.width, height: m.height,
            scale: (m.geometry_scale || 1),
            primary: (m === Main.layoutManager.primaryMonitor),
        }));
        return JSON.stringify({
            ok: true,
            input_backend: this._vkeyboard?.constructor?.name || 'unknown',
            // screen = the click/move coordinate space (logical stage pixels).
            screen: {width: stageW, height: stageH, x: 0, y: 0},
            // image is ADVISORY: logical stage dims. The PNG is captured in
            // PHYSICAL pixels, so on a fractional/HiDPI display (geometry_scale
            // != 1) these differ by the scale factor. The Rust side never trusts
            // this — it reads the true pixel space off the actual screenshot via
            // png_dimensions(), so the model always grounds coords on the real
            // image. Equal to the screenshot only at scale 1.0 (this machine).
            image: {width: stageW, height: stageH, scale: 1},
            monitors,
            user_pointer: {x: px, y: py},
            agent_cursor: {x: ax, y: ay},
            restore_pointer: this._restorePointer,
            window_batch: this._batch?.kind === 'window_batch' ? {phase: this._batch.phase} : null,
        });
    }

    // Window targeting (the "act on a specific window" capability). Uses only
    // stable Meta.Window getters + Main.activateWindow — no extra imports.
    ListWindows() {
        const windows = global.get_window_actors()
            .map(actor => actor.meta_window)
            .filter(w => w && !w.is_skip_taskbar())
            .map(w => {
                const r = w.get_frame_rect();
                return {
                    id: w.get_stable_sequence(),
                    title: w.get_title() || '',
                    app: w.get_wm_class() || '',
                    pid: w.get_pid(),
                    focused: w.has_focus(),
                    minimized: w.minimized,
                    x: r.x, y: r.y, width: r.width, height: r.height,
                };
            });
        return JSON.stringify({ok: true, windows});
    }

    FocusWindow(id) {
        this._requireMotionIdle();
        const win = global.get_window_actors()
            .map(actor => actor.meta_window)
            .find(w => w && w.get_stable_sequence() === id);
        if (!win) {
            return JSON.stringify({ok: false, error: `no window with id ${id}`});
        }
        // activateWindow un-minimizes, switches workspace, and raises+focuses.
        Main.activateWindow(win);
        return JSON.stringify({
            ok: true,
            focused: {id, title: win.get_title() || '', app: win.get_wm_class() || ''},
        });
    }

    // --- layered window control (act on a window even when it is covered) ---

    _windowById(id) {
        const actor = global.get_window_actors()
            .find(a => a.meta_window && a.meta_window.get_stable_sequence() === id);
        return actor ? {actor, win: actor.meta_window} : null;
    }

    /// Capture ONE window through Shell.Screenshot's C paint path — the same
    /// machinery GNOME's own screenshot tool uses. NEVER go back to
    /// MetaShapedTexture.get_image() here: it returns a cairo surface across
    /// the gjs boundary, and any internal NULL (unpainted first frame, the
    /// offscreen fallback on NVIDIA + fractional scaling, …) becomes an
    /// UNCATCHABLE exception that aborts gnome-shell — on Wayland that ends
    /// the user's whole session. It caused three logouts on 2026-07-03; JS
    /// try/catch and pre-checks provably cannot fence all its NULL paths.
    /// With screenshot_window() every failure surfaces as a catchable async
    /// error instead. Trade-off: it captures the FOCUSED window, so this
    /// raises the target first (activateWindow also un-minimizes).
    CaptureWindowAsync(params, invocation) {
        if (!this._admitPointerAction(invocation)) return;
        const [id, path] = params;
        const reply = obj =>
            invocation.return_value(GLib.Variant.new('(s)', [JSON.stringify(obj)]));
        const capture = {};
        this._batch = capture;
        const timers = new Set();
        let replied = false, inFlight = false, canceled = false;
        const clearTimers = () => {
            for (const timer of timers) {
                GLib.source_remove(timer);
                this._timeouts.delete(timer);
            }
            timers.clear();
        };
        const finish = result => {
            clearTimers();
            if (!inFlight && this._batch === capture) this._batch = null;
            if (!replied) { replied = true; reply(result); }
        };
        capture.cancel = reason => {
            canceled = true;
            // Shell's screenshot call has no cancellable argument. Once it
            // has entered native code, retain input ownership until its
            // callback drains; do not let another action change its focus.
            finish({ok: false, error: reason});
        };
        const current = () => this._batch === capture && !canceled;
        const later = (ms, fn) => {
            const timer = this._later(ms, () => {
                timers.delete(timer);
                if (current()) {
                    try { fn(); }
                    catch (error) { capture.cancel(`window capture failed: ${error.message}`); }
                }
            });
            timers.add(timer);
        };
        try {
            const found = this._windowById(id);
            if (!found) {
                finish({ok: false, error: `no window with id ${id}`});
                return;
            }
            const {win} = found;
            // Raising steals focus, so wait for a pause in the user's own
            // typing first — same courtesy WindowBatch extends.
            this._whenUserQuiet(() => {
            Main.activateWindow(win);
            // One beat for the compositor to focus and paint the raised
            // window; a window that still refuses focus gets a retryable
            // error, not a blind capture of whatever ended up on top.
            later(250, () => {
                let stream = null;
                let staging = null;
                let ownsStaging = false;
                const discardStaging = () => {
                    if (ownsStaging) {
                        try { staging.delete(null); ownsStaging = false; }
                        catch (error) { console.error(`Phoenix capture staging cleanup failed: ${error.message}`); }
                    }
                };
                try {
                    if (global.display.focus_window !== win) {
                        finish({ok: false, error: 'window did not take focus for capture — retry in a moment, or use computer_screenshot for the full desktop'});
                        return GLib.SOURCE_REMOVE;
                    }
                    const file = Gio.File.new_for_path(path);
                    // Native capture writes only its private staging file.
                    // A canceled callback must never recreate the caller's
                    // already-discarded destination or overwrite old output.
                    staging = Gio.File.new_for_path(`${path}.phoenix-${GLib.uuid_string_random()}.partial`);
                    stream = staging.create(Gio.FileCreateFlags.PRIVATE, null);
                    ownsStaging = true;
                    const shooter = new Shell.Screenshot();
                    later(10000, () => capture.cancel('window capture timed out; native capture is still draining'));
                    inFlight = true;
                    shooter.screenshot_window(true, false, stream, (_o, res) => {
                        let closed = false;
                        try {
                            const [, shotArea] = shooter.screenshot_window_finish(res);
                            stream.close(null);
                            closed = true;
                            inFlight = false;
                            if (canceled || this._batch !== capture) {
                                finish({ok: false, error: 'window capture canceled'});
                                return;
                            }
                            const buffer = win.get_buffer_rect();
                            const frame = win.get_frame_rect();
                            const area = shotArea || buffer;
                            const result = {
                                ok: true,
                                path,
                                title: win.get_title() || '',
                                app: win.get_wm_class() || '',
                                // `area` is the captured rect in global logical
                                // coords: image pixel (rx, ry) ≙ global
                                // (area.x+rx, area.y+ry) at scale 1. buffer/
                                // frame keep the WindowBatch-compatible shape.
                                area: {x: area.x, y: area.y, width: area.width, height: area.height},
                                buffer: {x: buffer.x, y: buffer.y, width: buffer.width, height: buffer.height},
                                frame: {
                                    x: frame.x - buffer.x, y: frame.y - buffer.y,
                                    width: frame.width, height: frame.height,
                                },
                            };
                            staging.move(file, Gio.FileCopyFlags.NONE, null, null);
                            ownsStaging = false;
                            finish(result);
                        } catch (e) {
                            inFlight = false;
                            finish({ok: false, error: `window capture failed: ${e.message}`});
                        } finally {
                            if (!closed) {
                                try {
                                    stream.close(null);
                                } catch (_) {
                                    // The Rust caller removes any partial file.
                                }
                            }
                            discardStaging();
                        }
                    });
                } catch (e) {
                    inFlight = false;
                    if (stream !== null) {
                        try {
                            stream.close(null);
                        } catch (_) {
                            // Preserve the original capture error.
                        }
                    }
                    discardStaging();
                    finish({ok: false, error: `window capture failed: ${e.message}`});
                }
                return GLib.SOURCE_REMOVE;
            });
            }, () => capture.cancel('Desktop busy: no verified pause in user input; retry later or use computer_screenshot without changing focus'), current, later);
        } catch (e) {
            finish({ok: false, error: `window capture failed: ${e.message}`});
        }
    }

    /// Push a window below the others — the agent's workspace stays on the
    /// desktop but out of the user's way; they reveal it by moving windows.
    LowerWindow(id) {
        this._requireMotionIdle();
        const found = this._windowById(id);
        if (!found)
            return JSON.stringify({ok: false, error: `no window with id ${id}`});
        try {
            found.win.lower();
            return JSON.stringify({ok: true, lowered: id});
        } catch (e) {
            return JSON.stringify({ok: false, error: e.message});
        }
    }

    /// ms since the user last touched keyboard/mouse (0 when unknowable).
    _userIdleMs() {
        try {
            return global.backend.get_core_idle_monitor().get_idletime();
        } catch (e) {
            return 0; // unknown user presence is not evidence of an idle desktop
        }
    }

    /// Run `fn` at the next natural pause in the user's own input (≥400ms
    /// quiet). If no pause is observed in five seconds, report busy rather
    /// than treating elapsed time as permission to interrupt live input.
    _whenUserQuiet(fn, onBusy, current = () => true, later = (ms, cb) => this._later(ms, cb)) {
        const started = Date.now();
        const poll = () => {
            if (!current()) return;
            if (this._userIdleMs() >= 400)
                fn();
            else if (Date.now() - started >= 5000)
                onBusy();
            else
                later(150, poll);
        };
        poll();
    }

    /// Execute a batch of window-relative actions against one window while the
    /// user keeps working: keyboard focus moves to the target for the batch
    /// (without switching workspaces), pointer actions briefly raise the
    /// window, and afterwards the user's focus window, stacking, and pointer
    /// are restored. Actions (JSON array, coords relative to the window's
    /// buffer rect, i.e. to a CaptureWindow image):
    ///   {type:"click", x, y, button?, double?} · {type:"type", text}
    ///   {type:"key", combo} · {type:"scroll", x, y, dx, dy} · {type:"wait", ms}
    WindowBatchAsync(params, invocation) {
        if (this._motion || this._batch || this._pointerAction) {
            invocation.return_dbus_error('dev.phoenix.Cursor.Busy', 'Another input operation is still active');
            return;
        }
        const [id, actionsJson] = params;
        const reply = obj => invocation.return_value(
            GLib.Variant.new('(s)', [JSON.stringify(obj)]));
        const found = this._windowById(id);
        if (!found)
            return reply({ok: false, error: `no window with id ${id}`});
        const {win} = found;
        let actions;
        try {
            actions = validateWindowActions(JSON.parse(actionsJson));
        } catch (e) {
            return reply({ok: false, executed: 0, error: `Rejected before execution: ${e.message}`});
        }

        const prevFocus = global.display.get_focus_window();
        const [userX, userY] = global.get_pointer();
        const needsPointer = actions.some(a =>
            a.type === 'move' || a.type === 'click' || a.type === 'scroll' || a.type === 'drag');
        const log = [];
        let settled = false, acquiredFocus = false;
        let completedActions = 0, focusChanged = false, dragPressed = false;
        let focusWatch = 0;
        const timers = new Set();
        const batch = {kind: 'window_batch', phase: 'acquiring_focus'};
        this._batch = batch;
        const later = (ms, fn) => {
            const id = this._later(ms, () => {
                timers.delete(id);
                if (this._batch === batch) fn();
            });
            timers.add(id);
        };

        const finish = ok => {
            if (settled) return;
            settled = true;
            if (focusWatch) {
                global.display.disconnect(focusWatch);
                focusWatch = 0;
            }
            for (const id of timers) {
                GLib.source_remove(id);
                this._timeouts.delete(id);
            }
            timers.clear();
            if (dragPressed) {
                try { this._vpointer.notify_button(GLib.get_monotonic_time(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.RELEASED); }
                catch (error) { ok = false; log.push(`drag release failed: ${error.message}`); }
                dragPressed = false;
            }
            // Restore the user's world: pointer back, their window back on
            // top and focused (unless the batch targeted the focused window).
            if (acquiredFocus) {
                try { this._pointerTo(userX, userY); }
                catch (error) { ok = false; log.push(`pointer restore failed: ${error.message}`); }
            }
            try {
                if (acquiredFocus && prevFocus && prevFocus !== win && prevFocus.get_workspace() &&
                    (typeof win.has_focus !== 'function' || win.has_focus())) {
                    prevFocus.raise();
                    prevFocus.focus(this._now());
                }
            } catch (e) {
                ok = false;
                log.push(`focus restore failed: ${e.message}`);
            }
            if (this._batch === batch) this._batch = null;
            reply({ok, steps: log, completed_actions: completedActions,
                total_actions: actions.length, focus_changed: focusChanged,
                shortened_waits: ok && focusChanged ? actions.length - completedActions : 0});
        };
        batch.cancel = reason => { log.push(`CANCELED: ${reason}`); finish(false); };
        const focusTransition = () => {
            focusChanged = true;
            // Opening/closing a dialog after the final input is normal. Do
            // not report undelivered input or send any input to the new focus.
            // An unfinished wait is reported explicitly, not claimed complete.
            if (completedActions > 0 && actions.slice(completedActions).every(a => a.type === 'wait')) {
                log.push('FOCUS_CHANGED after input; remaining waits ended');
                finish(true);
            } else batch.cancel('Target window lost keyboard focus');
        };

        const globalPoint = a => {
            const buffer = win.get_buffer_rect();
            return [buffer.x + (a.x | 0), buffer.y + (a.y | 0)];
        };
        const glidePointer = (gx, gy, onArrival) => {
            const [fromX, fromY] = this._cursorCenter();
            const distance = Math.hypot(gx - fromX, gy - fromY);
            const duration = movementDuration(distance, 0);
            this._showCursor();
            this._cursor.remove_all_transitions();
            this._cursor.opacity = 255;
            this._cursor.ease({
                x: gx - CURSOR_HOTSPOT,
                y: gy - CURSOR_HOTSPOT,
                duration,
                mode: Clutter.AnimationMode.EASE_IN_OUT_CUBIC,
            });
            // WindowBatch owns the pointer for the whole sequence. Delay the
            // actual click/move until the visible Phoenix cursor arrives, so
            // selections read as one quick human-like glide instead of a
            // teleport followed by a ripple. The timer is batch-owned and is
            // canceled automatically on focus loss/cancellation.
            later(duration, () => {
                if (Main.overview?.visible || (typeof win.has_focus === 'function' && !win.has_focus()))
                    return focusTransition();
                this._cursor.set_position(gx - CURSOR_HOTSPOT, gy - CURSOR_HOTSPOT);
                this._pointerTo(gx, gy);
                onArrival();
            });
        };

        const step = i => {
            if (this._batch !== batch || settled) return;
            if (i >= actions.length)
                return finish(true);
            if (Main.overview?.visible ||
                (typeof win.has_focus === 'function' && !win.has_focus())) {
                return focusTransition();
            }
            const a = actions[i];
            batch.phase = a.type === 'wait' ? 'waiting' : 'executing';
            try {
                switch (a.type) {
                case 'move': {
                    const buffer = win.get_buffer_rect();
                    if (a.x >= buffer.width || a.y >= buffer.height)
                        throw new Error('move target is outside the target window');
                    const [gx, gy] = globalPoint(a);
                    glidePointer(gx, gy, () => {
                        log.push(`move (${a.x}, ${a.y})`);
                        completedActions = i + 1;
                        later(60, () => step(i + 1));
                    });
                    return;
                }
                case 'click': {
                    const [gx, gy] = globalPoint(a);
                    const btn = BUTTONS[a.button || 'left'] ?? Clutter.BUTTON_PRIMARY;
                    glidePointer(gx, gy, () => {
                        this._spawnRipple(gx, gy, !!a.double);
                        this._pressRelease(btn);
                        if (a.double)
                            this._pressRelease(btn);
                        log.push(`click (${a.x}, ${a.y})`);
                        completedActions = i + 1;
                        later(60, () => step(i + 1));
                    });
                    return;
                }
                case 'drag': {
                    const buffer = win.get_buffer_rect();
                    for (const [x, y] of [[a.from_x, a.from_y], [a.to_x, a.to_y]]) {
                        if (x >= buffer.width || y >= buffer.height)
                            throw new Error('drag endpoint is outside the target window');
                    }
                    const [fromX, fromY] = globalPoint({x:a.from_x, y:a.from_y});
                    const [toX, toY] = globalPoint({x:a.to_x, y:a.to_y});
                    const duration = a.duration_ms ?? 300;
                    let started;
                    const [pointerX, pointerY] = global.get_pointer();
                    // A modal selection can already own a relative pointer
                    // grab before the button press. Move its start through the
                    // same device stream, so the client sees that position.
                    this._vpointer.notify_relative_motion(GLib.get_monotonic_time(), fromX - pointerX, fromY - pointerY);
                    this._cursor.set_position(fromX - CURSOR_HOTSPOT, fromY - CURSOR_HOTSPOT);
                    this._showCursor();
                    let lastX = fromX, lastY = fromY;
                    const advance = () => {
                        if (settled || this._batch !== batch) return;
                        if (Main.overview?.visible || (typeof win.has_focus === 'function' && !win.has_focus()))
                            return focusTransition();
                        try {
                            const progress = duration === 0 ? 1 : Math.min(1, (GLib.get_monotonic_time() - started) / (duration * 1000));
                            const x = Math.round(fromX + (toX - fromX) * progress);
                            const y = Math.round(fromY + (toY - fromY) * progress);
                            this._cursor.set_position(x - CURSOR_HOTSPOT, y - CURSOR_HOTSPOT);
                            // Warping the seat during a held-button gesture does
                            // not deliver usable drag motion to Blender. Send the
                            // actual device deltas while preserving the grab.
                            this._vpointer.notify_relative_motion(GLib.get_monotonic_time(), x - lastX, y - lastY);
                            lastX = x; lastY = y;
                            if (progress < 1) return later(16, advance);
                            this._vpointer.notify_button(GLib.get_monotonic_time(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.RELEASED);
                            dragPressed = false;
                            log.push(`drag (${a.from_x}, ${a.from_y}) → (${a.to_x}, ${a.to_y})`);
                            completedActions = i + 1;
                            later(60, () => step(i + 1));
                        } catch (error) {
                            log.push(`drag failed: ${error.message}`);
                            finish(false);
                        }
                    };
                    // Let the initial move reach the client before pressing.
                    // Otherwise Blender starts its box at the previous pointer
                    // position even though the later drag path is correct.
                    later(16, () => {
                        if (Main.overview?.visible || (typeof win.has_focus === 'function' && !win.has_focus()))
                            return focusTransition();
                        try {
                            started = GLib.get_monotonic_time();
                            dragPressed = true;
                            this._vpointer.notify_button(started, Clutter.BUTTON_PRIMARY, Clutter.ButtonState.PRESSED);
                            later(16, advance);
                        } catch (error) {
                            log.push(`drag press failed: ${error.message}`);
                            finish(false);
                        }
                    });
                    return;
                }
                case 'scroll': {
                    const [gx, gy] = globalPoint(a);
                    this._pointerTo(gx, gy);
                    const emit = (count, dirPos, dirNeg) => {
                        const dir = count > 0 ? dirPos : dirNeg;
                        const n = Math.min(30, Math.abs(count));
                        for (let k = 0; k < n; k++)
                            this._vpointer.notify_discrete_scroll(
                                GLib.get_monotonic_time(), dir, Clutter.ScrollSource.WHEEL);
                    };
                    if (a.dy) emit(a.dy, Clutter.ScrollDirection.DOWN, Clutter.ScrollDirection.UP);
                    if (a.dx) emit(a.dx, Clutter.ScrollDirection.RIGHT, Clutter.ScrollDirection.LEFT);
                    log.push(`scroll (${a.dx | 0}, ${a.dy | 0})`);
                    break;
                }
                case 'type':
                    if (this._ownedDesktop) this._pointerTo(...this._cursorCenter());
                    this._typeText(String(a.text ?? ''));
                    log.push(`type ${String(a.text ?? '').length} chars`);
                    break;
                case 'key':
                    if (this._ownedDesktop) this._pointerTo(...this._cursorCenter());
                    this._key(String(a.combo ?? ''));
                    log.push(`key ${a.combo}`);
                    break;
                case 'wait':
                    log.push(`wait ${a.ms | 0}ms`);
                    return later(Math.max(0, Math.min(a.ms | 0, 10000)), () => {
                        completedActions = i + 1;
                        step(i + 1);
                    });
                default:
                    throw new Error(`unsupported action type: ${a.type}`);
                }
            } catch (e) {
                log.push(`FAILED at #${i + 1} (${a.type}): ${e.message}`);
                return finish(false);
            }
            completedActions = i + 1;
            later(60, () => step(i + 1));
        };

        // Wait for a pause in the user's own input, take keyboard focus
        // (raise only if pointer actions need the window hittable), act,
        // then restore. The user's windows stay where they are.
        this._whenUserQuiet(() => {
            try {
                acquiredFocus = true;
                if (Main.overview?.visible)
                    Main.overview.hide();
                if (needsPointer)
                    win.raise();
                win.focus(this._now());
            } catch (e) {
                log.push(`focus/raise failed: ${e.message}`);
                return finish(false);
            }
            const focusDeadline = GLib.get_monotonic_time() + 2_000_000;
            const beginWhenFocused = () => {
                if (Main.overview?.visible ||
                    (typeof win.has_focus === 'function' && !win.has_focus())) {
                    if (GLib.get_monotonic_time() >= focusDeadline) {
                        log.push('FAILED before input: target window did not acquire keyboard focus');
                        return finish(false);
                    }
                    return later(16, beginWhenFocused);
                }
                if (typeof global.display.connect === 'function') {
                    focusWatch = global.display.connect('notify::focus-window', () => {
                        if (this._batch === batch && global.display.get_focus_window() !== win)
                            focusTransition();
                    });
                }
                step(0);
            };
            later(80, beginWhenFocused);
        }, () => batch.cancel('Desktop busy: no verified pause in user input; retry later'),
            () => this._batch === batch, later);
    }

    MoveToAsync(params, invocation) {
        const [x, y, durationMs] = params;
        if (this._motion || this._batch || this._pointerAction) {
            invocation.return_dbus_error('dev.phoenix.Cursor.Busy', 'Another cursor motion is still active');
            return;
        }
        try {
            this._animateTo(x, y, durationMs, () => invocation.return_value(null),
                reason => invocation.return_dbus_error('dev.phoenix.Cursor.Canceled', reason));
        } catch (error) {
            invocation.return_dbus_error('dev.phoenix.Cursor.Canceled', error.message);
        }
    }

    ClickAsync([button], invocation) {
        if (!this._admitPointerAction(invocation)) return;
        const btn = BUTTONS[button] ?? Clutter.BUTTON_PRIMARY;
        const [x, y] = this._cursorCenter();
        this._spawnRipple(x, y, false);
        this._withPointerAt(x, y, () => this._pressRelease(btn), invocation);
    }

    DoubleClickAsync([button], invocation) {
        if (!this._admitPointerAction(invocation)) return;
        const btn = BUTTONS[button] ?? Clutter.BUTTON_PRIMARY;
        const [x, y] = this._cursorCenter();
        this._spawnRipple(x, y, true);
        this._withPointerAt(x, y, () => {
            this._pressRelease(btn);
            this._pressRelease(btn);
        }, invocation);
    }

    DragAsync(params, invocation) {
        if (this._motion || this._batch || this._pointerAction) {
            invocation.return_dbus_error('dev.phoenix.Cursor.Busy', 'Another cursor motion is still active');
            return;
        }
        const [toX, toY, durationMs] = params;
        const [fromX, fromY] = this._cursorCenter();
        const [userX, userY] = global.get_pointer();
        let dragId = 0, pressed = false, settled = false;
        let lastX = fromX, lastY = fromY;
        const moveGrab = (x, y) => {
            this._vpointer.notify_relative_motion(GLib.get_monotonic_time(), x - lastX, y - lastY);
            lastX = x; lastY = y;
        };
        const finish = reason => {
            if (settled) return;
            settled = true;
            if (dragId) {
                GLib.source_remove(dragId);
                this._timeouts.delete(dragId);
            }
            try {
                if (pressed) this._vpointer.notify_button(GLib.get_monotonic_time(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.RELEASED);
            } catch (error) { reason = reason || error.message; }
            try {
                // Restore now, not from a delayed callback that could steal
                // the pointer back from the next admitted operation.
                if (this._restorePointer) this._pointerTo(userX, userY);
            } catch (error) { reason = reason || error.message; }
            if (reason) invocation.return_dbus_error('dev.phoenix.Cursor.Canceled', reason);
            else invocation.return_value(null);
        };
        try {
            this._pointerTo(fromX, fromY);
            pressed = true;
            this._vpointer.notify_button(GLib.get_monotonic_time(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.PRESSED);
            this._spawnRipple(fromX, fromY, false);
            this._animateTo(toX, toY, durationMs, () => {
                try {
                    moveGrab(toX, toY);
                    this._spawnRipple(toX, toY, false);
                    finish();
                } catch (error) { finish(error.message); }
            }, finish);
        } catch (error) { finish(error.message); }
        if (settled) return;
        // Drag the real grab along the animated path so apps see the motion.
        dragId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 30, () => {
            if (!this._cursor || settled) {
                return GLib.SOURCE_REMOVE;
            }
            try {
                const [cx, cy] = this._cursorCenter();
                moveGrab(cx, cy);
                return GLib.SOURCE_CONTINUE;
            } catch (error) {
                this._cancelMotion(error.message);
                return GLib.SOURCE_REMOVE;
            }
        });
        this._timeouts.add(dragId);
    }

    ScrollAsync([dx, dy], invocation) {
        if (!this._admitPointerAction(invocation)) return;
        const [x, y] = this._cursorCenter();
        this._withPointerAt(x, y, () => {
            // Clutter's signature is notify_discrete_scroll(time, direction,
            // scroll_source) — the previous call passed (time, x, y, dir), so
            // `direction` got an x-pixel value (not a valid enum) and nothing
            // scrolled. Emit one event per notch with the correct argument order.
            const notches = n => {
                const a = Math.abs(n);
                // Accept wheel notches (small) or pixel-ish deltas (large,
                // ~120px per notch), capped so one call can't scroll forever.
                return Math.min(30, a > 30 ? Math.max(1, Math.round(a / 120)) : a);
            };
            const emit = (count, dirPos, dirNeg) => {
                const dir = count > 0 ? dirPos : dirNeg;
                const n = notches(count);
                for (let i = 0; i < n; i++)
                    this._vpointer.notify_discrete_scroll(
                        GLib.get_monotonic_time(), dir, Clutter.ScrollSource.WHEEL);
            };
            if (dy !== 0)
                emit(dy, Clutter.ScrollDirection.DOWN, Clutter.ScrollDirection.UP);
            if (dx !== 0)
                emit(dx, Clutter.ScrollDirection.RIGHT, Clutter.ScrollDirection.LEFT);
        }, invocation);
    }

    _directTypeText(text) {
        this._requireMotionIdle();
        this._typeText(text);
    }

    TypeTextAsync([text], invocation) {
        this._keyboardAtAgentCursor(() => this._typeText(text), invocation);
    }

    _keyboardAtAgentCursor(action, invocation) {
        if (!this._admitPointerAction(invocation)) return;
        if (this._ownedDesktop) {
            const [x,y] = this._cursorCenter();
            this._withPointerAt(x,y,action,invocation);
        } else {
            try {action();invocation.return_value(null);}
            catch(error) {invocation.return_dbus_error('dev.phoenix.Cursor.InputFailed',error.message);}
        }
    }

    _typeText(text) {
        this._showCursor();
        if (/[^\x00-\x7f]/.test(text)) {
            // Raw virtual keyvals only cover symbols in the active keymap;
            // Mutter can drop unmapped characters without throwing. Use the
            // same focused text-input commit path as GNOME's on-screen
            // keyboard. Never touch the user's clipboard to work around it.
            if (!Main.inputMethod?.currentFocus)
                throw new Error('Unicode text requires a focused text-input target; no text was entered');
            Main.inputMethod.commit(text);
            return;
        }
        for (const ch of text) {
            if (ch === '\n') {
                this._keyvalTap(KEYSYMS.enter);
                continue;
            }
            const cp = ch.codePointAt(0);
            this._keyvalTap(cp < 0x80 ? cp : 0x01000000 + cp);
        }
    }

    _directKey(combo) {
        this._requireMotionIdle();
        this._key(combo);
    }

    KeyAsync([combo], invocation) {
        this._keyboardAtAgentCursor(() => this._key(combo), invocation);
    }

    _key(combo) {
        const {mods, main} = parseKeyCombo(combo);
        this._showCursor();
        this._withHeldKeys(mods, () => this._keyvalTap(main));
    }

    ScreenshotAsync(params, invocation) {
        const [path] = params;
        let stream = null, staging = null, ownsStaging = false;
        let replied = false, canceled = false, deadline = 0;
        const shot = {};
        this._screenshots ??= new Set();
        this._screenshots.add(shot);
        const reply = error => {
            if (deadline) {
                GLib.source_remove(deadline);
                this._timeouts.delete(deadline);
                deadline = 0;
            }
            if (!replied) {
                replied = true;
                invocation.return_value(GLib.Variant.new('(s)', [error ? `error: ${error}` : 'ok']));
            }
        };
        shot.cancel = reason => { canceled = true; reply(reason); };
        const cleanup = () => {
            if (ownsStaging) {
                try { staging.delete(null); ownsStaging = false; }
                catch (error) { console.error(`Phoenix screenshot staging cleanup failed: ${error.message}`); }
            }
            this._screenshots.delete(shot);
        };
        try {
            const file = Gio.File.new_for_path(path);
            staging = Gio.File.new_for_path(`${path}.phoenix-${GLib.uuid_string_random()}.partial`);
            stream = staging.create(Gio.FileCreateFlags.PRIVATE, null);
            ownsStaging = true;
            const shooter = new Shell.Screenshot();
            deadline = this._later(10000, () => {
                deadline = 0;
                shot.cancel('screenshot timed out; native capture is still draining');
            });
            shooter.screenshot(false, stream, (_o, res) => {
                let closed = false;
                try {
                    shooter.screenshot_finish(res);
                    stream.close(null);
                    closed = true;
                    if (canceled) return;
                    staging.move(file, Gio.FileCopyFlags.NONE, null, null);
                    ownsStaging = false;
                    reply(null);
                } catch (e) {
                    reply(e.message);
                } finally {
                    if (!closed) {
                        try {
                            stream.close(null);
                        } catch (_) {
                            // The Rust caller removes any partial file.
                        }
                    }
                    cleanup();
                }
            });
        } catch (e) {
            if (stream !== null) {
                try {
                    stream.close(null);
                } catch (_) {
                    // Preserve the original capture error.
                }
            }
            cleanup();
            reply(e.message);
        }
    }

    SetRestorePointer(restore) {
        this._restorePointer = restore;
    }

    Hide() {
        for (const shot of this._screenshots || []) shot.cancel('Cursor hidden');
        this._pointerAction?.finish('Cursor hidden');
        this._batch?.cancel('Cursor hidden');
        this._cancelMotion('Cursor hidden');
        this._cursor?.ease({
            opacity: 0,
            duration: 300,
            mode: Clutter.AnimationMode.EASE_IN_QUAD,
        });
    }
}
