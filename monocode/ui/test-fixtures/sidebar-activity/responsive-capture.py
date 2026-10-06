"""Capture actual WebKit layout at 820/1024/1440 via the existing preview18796.

Run from product root: xvfb-run -a python3 canvas-app/ui/test-fixtures/sidebar-activity/responsive-capture.py before
"""
import json
import pathlib
import sys
import gi

gi.require_version('Gtk', '3.0')
gi.require_version('Gdk', '3.0')
gi.require_version('WebKit2', '4.1')
from gi.repository import Gtk, Gdk, GLib, WebKit2

phase = sys.argv[1] if len(sys.argv) == 2 else 'after'
if phase not in ('before', 'after'):
    raise SystemExit('Expected before or after')
out = pathlib.Path('artifacts/ui-navigation-2026-09-17-worker-1') / ('responsive-' + phase)
out.mkdir(exist_ok=True)
widths = [820, 1024, 1440]
index = 0
reports = []
failed = False
started = False
window = Gtk.Window()
window.set_default_size(widths[0], 920)
view = WebKit2.WebView.new_with_context(WebKit2.WebContext.new_ephemeral())
window.add(view)
window.show_all()

def fail(message):
    global failed
    failed = True
    (out / 'failure.txt').write_text(str(message))
    print('ERROR:', message, flush=True)
    Gtk.main_quit()

def js(script, callback):
    def done(widget, result, _data):
        try:
            callback(widget.run_javascript_finish(result).get_js_value().to_string())
        except Exception as error:
            fail(error)
    view.run_javascript(script, None, done, None)

def poll():
    def done(value):
        if value == 'null':
            GLib.timeout_add(100, poll)
        else:
            result = json.loads(value)
            if 'error' in result:
                fail(result['error'])
            else:
                reports.append(result)
                (out / f'webkit-{widths[index]}.json').write_text(json.dumps(result, indent=2) + '\n')
                print(f"{phase} {widths[index]}: {sum(c['ok'] for c in result['checks'])}/{len(result['checks'])}", flush=True)
                GLib.timeout_add(400, capture)
    js('JSON.stringify(window.responsiveReport || null)', done)
    return False

def capture():
    global index
    image = Gdk.pixbuf_get_from_window(window.get_window(), 0, 0, window.get_allocated_width(), window.get_allocated_height())
    if image is None:
        fail('Missing rendered WebKit image')
        return False
    image.savev(str(out / f'webkit-{widths[index]}.png'), 'png', [], [])
    index += 1
    if index < len(widths):
        run_width()
    else:
        (out / 'results.json').write_text(json.dumps(reports, indent=2) + '\n')
        Gtk.main_quit()
    return False

def run_width():
    window.resize(widths[index], 920)
    def run():
        js("window.responsiveReport=null; checkResponsiveFixture(true).then(result=>window.responsiveReport=result).catch(error=>window.responsiveReport={error:String(error)}); 'running'", lambda _: poll())
        return False
    GLib.timeout_add(300, run)

def loaded(_view, event):
    global started
    if event != WebKit2.LoadEvent.FINISHED or started:
        return
    started = True
    js("""window.responsiveReady=false; (async()=>{
      for(const file of ['checks.js','responsive.js'])await new Promise((resolve,reject)=>{
        const s=document.createElement('script');s.src='test-fixtures/sidebar-activity/'+file;
        s.onload=resolve;s.onerror=reject;document.head.append(s);
      });await prepareResponsiveFixture();window.responsiveReady=true;
    })().catch(error=>window.responsiveReady=String(error));'loading'""", lambda _: GLib.timeout_add(800, ready))

def ready():
    def done(value):
        if value == 'true':
            run_width()
        elif value == 'false':
            GLib.timeout_add(100, ready)
        else:
            fail('Fixture failed: ' + value)
    js('String(window.responsiveReady)', done)
    return False

view.connect('load-changed', loaded)
view.load_uri('http://127.0.0.1:18796/index.html?shot=group&responsive-webkit=worker-1')
GLib.timeout_add_seconds(40, lambda: (fail('Capture deadline exceeded'), False)[1])
Gtk.main()
window.destroy()
sys.exit(1 if failed or (phase == 'after' and not all(r['ok'] for r in reports)) else 0)
