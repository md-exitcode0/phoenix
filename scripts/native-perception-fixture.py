"""Disposable native window for testing real screen reading without a sidecar."""
import os
import gi
gi.require_version('Gtk', '4.0')
from gi.repository import Gtk

def activate(app):
    window = Gtk.ApplicationWindow(application=app, title='Phoenix Perception Check')
    window.set_default_size(700, 400)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=28)
    for name in ['top', 'bottom', 'start', 'end']:
        getattr(box, 'set_margin_' + name)(32)
    label = Gtk.Label(label='Verification code: ' + os.environ['PHOENIX_PERCEPTION_CODE'])
    label.set_selectable(False)
    box.append(label)
    box.append(Gtk.Label(label='Read this window through native vision.'))
    button = Gtk.Button(label='Confirm sample')
    button.add_css_class('suggested-action')
    box.append(button)
    window.set_child(box)
    window.present()

app = Gtk.Application(application_id='dev.phoenix.PerceptionFixture')
app.connect('activate', activate)
app.run(None)
