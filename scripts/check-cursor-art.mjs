import Cairo from 'cairo';
import GLib from 'gi://GLib';
import {paintCursor} from '../desktop/gnome-extension/phoenix-cursor@phoenix.dev/cursor-art.mjs';

// Renders the neon cursor art exactly as the GNOME extension does, on light
// and dark, with a cyan dot on the hotspot. Usage: gjs -m this.mjs out.png
const [out = '/dev/null'] = ARGV;
const dir = `${GLib.path_get_dirname(GLib.filename_from_uri(import.meta.url)[0])}/../desktop/gnome-extension/phoenix-cursor@phoenix.dev/`;
const art = Cairo.ImageSurface.createFromPNG(`${dir}cursor.png`);
if (!(art.getWidth() > 0)) throw new Error('cursor art did not load');
const surface = new Cairo.ImageSurface(Cairo.Format.ARGB32, 240, 120);
const cr = new Cairo.Context(surface);
for (const [x, light] of [[0, true], [120, false]]) {
    cr.setSourceRGB(...(light ? [0.97, 0.96, 0.93] : [0.1, 0.1, 0.1]));
    cr.rectangle(x, 0, 120, 120); cr.fill();
    cr.save(); cr.translate(x + 30, 30); cr.scale(2, 2);
    paintCursor(cr, 10, art);
    cr.restore();
    cr.setSourceRGB(0, 0.9, 1); cr.arc(x + 30 + 20, 30 + 20, 1.5, 0, 2 * Math.PI); cr.fill();
}
surface.writeToPNG(out);
print('CURSOR_ART_OK');
