import Cairo from 'cairo';
import {paintCursor} from '../desktop/gnome-extension/phoenix-cursor@phoenix.dev/cursor-art.mjs';

// Native Cairo rendering without GNOME Shell or any input injection.
const surface = new Cairo.ImageSurface(Cairo.Format.ARGB32, 440, 180);
const cr = new Cairo.Context(surface);
for (const [x, light] of [[0,true],[220,false]]) {
    cr.setSourceRGB(...(light ? [0.97,0.96,0.93] : [0.14,0.14,0.13]));
    cr.rectangle(x,0,220,180);
    cr.fill();
    cr.save();
    cr.translate(x+18,35);
    paintCursor(cr);
    cr.restore();
    cr.save();
    cr.translate(x+88,25);
    cr.scale(3,3);
    paintCursor(cr);
    cr.restore();
}
surface.writeToPNG('artifacts/cursor-native-cairo-after.png');
cr.$dispose();
print('CURSOR_CAIRO_OK: production painter rendered with native Cairo on light/dark backgrounds');
