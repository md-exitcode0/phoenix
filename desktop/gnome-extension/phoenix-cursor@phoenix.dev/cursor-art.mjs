// Shared geometry for Cairo production rendering and the visual review page.
// The origin is the exact action hotspot, not the center of the drawing.
export const ARROW = Object.freeze([
    [0, 0], [1, 20], [6.1, 15.8], [10.2, 24],
    [14, 22], [9.8, 14], [17, 13.7],
].map(Object.freeze));

export function movementDuration(distance, requested = 0) {
    if (requested > 0) return requested;
    return Math.max(60, Math.min(180, Math.round(50 + Math.sqrt(Math.max(0, distance)) * 3)));
}

// The neon ember arrow art (cursor.png). Its tip sits at these fractions of
// the image, and it is drawn this tall in logical pixels.
export const ART_TIP = Object.freeze([0.0765, 0.0202]);
export const ART_HEIGHT = 34;

export function paintCursor(cr, hotspot = 10, art = null) {
    if (art) {
        const scale = ART_HEIGHT / art.getHeight();
        cr.translate(hotspot - ART_TIP[0] * art.getWidth() * scale, hotspot - ART_TIP[1] * art.getHeight() * scale);
        cr.scale(scale, scale);
        cr.setSourceSurface(art, 0, 0);
        cr.paint();
        return;
    }
    cr.translate(hotspot, hotspot);
    cr.moveTo(...ARROW[0]);
    for (const point of ARROW.slice(1)) cr.lineTo(...point);
    cr.closePath();
    cr.setLineJoin(1);
    // Charcoal outer edge holds on white; ivory inner edge holds on black.
    cr.setSourceRGBA(0.12, 0.10, 0.10, 0.96);
    cr.setLineWidth(3.2);
    cr.strokePreserve();
    cr.setSourceRGBA(0.94, 0.39, 0.23, 1);
    cr.fillPreserve();
    cr.setSourceRGBA(1, 0.94, 0.84, 1);
    cr.setLineWidth(0.9);
    cr.stroke();
    // One quiet folded-wing facet, rather than a glow over the target text.
    cr.moveTo(1.6, 2.9);
    cr.lineTo(14.4, 13.0);
    cr.lineTo(8.7, 12.8);
    cr.closePath();
    cr.setSourceRGBA(1, 0.72, 0.43, 1);
    cr.fill();
}
