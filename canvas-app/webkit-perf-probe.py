#!/usr/bin/env python3
"""Canvas renderer benchmark — measures the app's UI in a real WebKitGTK
window (the same engine the Tauri app uses) and prints frame rates.

Usage (from canvas-app/):
  python3 webkit-perf-probe.py "file://$PWD/ui/index.html?shot=perf" baseline

Compare renderer configs by prefixing env vars, e.g. after an NVIDIA driver
or libwebkit2gtk upgrade, test whether GPU compositing works yet:
  GDK_BACKEND=wayland WEBKIT_FORCE_DMABUF_RENDERER=1 PROBE_HW_ALWAYS=1 \
    python3 webkit-perf-probe.py "file://$PWD/ui/index.html?shot=perf" gpu

Findings 2026-07-11 (WebKitGTK 2.52.3, NVIDIA 595.71.05, RTX 4050 only-GPU,
scale 2): forcing the DMABUF (GPU) renderer crashes the Wayland connection
(protocol error 71) with and without GBM — GPU compositing is broken on the
proprietary driver, WebKit auto-falls back to CPU rendering. All configs pan
at ~14-16fps (full-viewport CPU repaint at 3840x2400); partial-damage drags
~50fps; rAF ceiling ~50fps (VBlank=Timer). If a probe run stops crashing and
pan reaches ~50+, remove WEBKIT_DISABLE_DMABUF_RENDERER/GDK_BACKEND from
src/main.rs and enjoy the GPU.
"""
import gi, sys, os
gi.require_version("Gtk", "3.0")
gi.require_version("WebKit2", "4.1")
from gi.repository import Gtk, WebKit2, GLib

url = sys.argv[1]
label = sys.argv[2] if len(sys.argv) > 2 else "run"

win = Gtk.Window(title=f"phoenix perf probe — {label}")
win.set_default_size(1400, 900)
wv = WebKit2.WebView()
if os.environ.get("PROBE_HW_ALWAYS"):
    s = wv.get_settings()
    s.set_hardware_acceleration_policy(WebKit2.HardwareAccelerationPolicy.ALWAYS)
win.add(wv)
win.show_all()
wv.load_uri(url)

def poll():
    t = wv.get_title()
    if t and t.startswith("PERF"):
        print(f"{label}: {t}", flush=True)
        Gtk.main_quit()
        return False
    return True

GLib.timeout_add(250, poll)
GLib.timeout_add_seconds(45, Gtk.main_quit)
win.connect("destroy", Gtk.main_quit)
Gtk.main()
