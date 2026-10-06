"""Run the sidebar fixture in an ephemeral WebKit window against preview18796.

From the product root:
  xvfb-run -a python3 canvas-app/ui/test-fixtures/sidebar-activity/capture.py
Only writes the worker's dated evidence; no gateway or model connection.
"""
import json
import pathlib
import sys

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
gi.require_version("WebKit2", "4.1")
from gi.repository import Gdk, GLib, Gtk, WebKit2

OUT = pathlib.Path("artifacts/ui-navigation-2026-09-17-worker-1")
OUT.mkdir(parents=True, exist_ok=True)
MODES = [("light", "light", 1440, 1000), ("dark", "dark", 1440, 1000), ("narrow", "light", 820, 920)]
window = Gtk.Window()
window.set_default_size(1440, 1000)
context = WebKit2.WebContext.new_ephemeral()
view = WebKit2.WebView.new_with_context(context)
window.add(view)
window.show_all()
reports = []
index = 0
failed = False
started = False
poll_count = 0


def fail(message):
    global failed
    failed = True
    print(message, flush=True)
    (OUT / "native-failure.txt").write_text(message + "\n")
    Gtk.main_quit()


def evaluate(script, callback):
    def done(widget, result, _data):
        try:
            value = widget.run_javascript_finish(result).get_js_value().to_string()
            callback(value)
        except Exception as error:
            fail(str(error))
    view.run_javascript(script, None, done, None)


def run_mode():
    global poll_count
    name, theme, width, height = MODES[index]
    window.resize(width, height)
    poll_count = 0
    script = """
      window.__sidebarCapture = null;
      window.PhoenixUI.setTheme(THEME);
      document.documentElement.dataset.motion = 'minimal';
      document.body.classList.remove('sidebar-collapsed');
      window.runSidebarAcceptance().then(result => {
        window.__sidebarCapture = {result, labels:[...document.querySelectorAll('.company-row')].map(row => ({id:row.dataset.id, text:row.querySelector('.row-title').textContent, tone:row.dataset.activityTone}))};
      }).catch(error => {window.__sidebarCapture = {error:String(error)}});
      'started';
    """.replace("THEME", json.dumps(theme))
    GLib.timeout_add(200, lambda: (evaluate(script, lambda _value: poll()), False)[1])
    return False


def poll():
    evaluate("JSON.stringify(window.__sidebarCapture || null)", polled)


def polled(value):
    global poll_count
    result = json.loads(value)
    if result is None:
        poll_count += 1
        if poll_count > 120:
            fail("Sidebar fixture timed out")
        else:
            GLib.timeout_add(100, lambda: (poll(), False)[1])
        return
    if "error" in result:
        fail(result["error"])
        return
    name = MODES[index][0]
    reports.append({"mode": name, **result})
    (OUT / f"native-{name}.json").write_text(json.dumps(result, indent=2) + "\n")
    print(f"{name}: {sum(c['ok'] for c in result['result']['checks'])}/{len(result['result']['checks'])} checks", flush=True)
    GLib.timeout_add(500, check_capture_state)


def check_capture_state():
    def checked(value):
        theme = json.loads(value)
        if theme["theme"] != MODES[index][1]:
            fail("Theme changed before capture: " + value)
            return
        reports[-1]["capture"] = theme
        name = MODES[index][0]
        (OUT / f"native-{name}.json").write_text(json.dumps(reports[-1], indent=2) + "\n")
        capture()
    evaluate("JSON.stringify({theme:document.documentElement.dataset.theme,width:innerWidth,sidebarBackground:getComputedStyle(document.getElementById('companySidebar')).backgroundColor})", checked)
    return False


def capture():
    global index, failed
    name = MODES[index][0]
    pixbuf = Gdk.pixbuf_get_from_window(window.get_window(), 0, 0, window.get_allocated_width(), window.get_allocated_height())
    if pixbuf is None:
        fail("Could not capture the rendered WebKit window")
        return False
    pixbuf.savev(str(OUT / f"sidebar-{name}.png"), "png", [], [])
    failed = failed or not reports[-1]["result"]["ok"]
    index += 1
    if index < len(MODES):
        run_mode()
    else:
        evaluate("document.body.classList.add('sidebar-collapsed'); 'collapsed'", lambda _: GLib.timeout_add(400, capture_collapsed))
    return False


def capture_collapsed():
    pixbuf = Gdk.pixbuf_get_from_window(window.get_window(), 0, 0, window.get_allocated_width(), window.get_allocated_height())
    if pixbuf is None:
        fail("Could not capture collapsed sidebar")
        return False
    pixbuf.savev(str(OUT / "sidebar-collapsed.png"), "png", [], [])
    (OUT / "native-results.json").write_text(json.dumps(reports, indent=2) + "\n")
    Gtk.main_quit()
    return False


def loaded(_view, event):
    global started
    if event != WebKit2.LoadEvent.FINISHED or started:
        return
    started = True
    script = """
      const fixtureScript = document.createElement('script');
      fixtureScript.src = 'test-fixtures/sidebar-activity/checks.js';
      fixtureScript.onload = () => {window.__sidebarFixtureReady = true};
      fixtureScript.onerror = () => {window.__sidebarFixtureReady = 'load failed'};
      document.head.append(fixtureScript);
      'loading';
    """
    evaluate(script, lambda _: GLib.timeout_add(1000, ready))


def ready():
    evaluate("String(window.__sidebarFixtureReady)", lambda value: run_mode() if value == "true" else fail("Fixture unavailable: " + value))
    return False


view.connect("load-changed", loaded)
view.load_uri("http://127.0.0.1:18796/index.html?shot=group&sidebar-native-worker=1")
GLib.timeout_add_seconds(40, lambda: (fail("Native capture deadline exceeded"), False)[1])
Gtk.main()
window.destroy()
sys.exit(1 if failed else 0)
