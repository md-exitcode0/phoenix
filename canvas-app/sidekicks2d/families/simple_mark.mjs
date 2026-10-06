const EYE_STYLES = new Set(['round', 'spark', 'slit', 'visor', 'closed']);
const EXPRESSIONS = new Set([
  'bright', 'joy', 'calm', 'curious', 'mischief', 'sleepy', 'focused', 'determined',
]);

const DEFAULT_COLOR = '#e55732';

function safeColor(value) {
  const text = String(value || '').trim();
  if (/^#[0-9a-f]{6}$/i.test(text)) return text.toLowerCase();
  if (/^#[0-9a-f]{3}$/i.test(text)) {
    const [r, g, b] = text.slice(1).split('');
    return `#${r}${r}${g}${g}${b}${b}`.toLowerCase();
  }
  return DEFAULT_COLOR;
}

function safeUid(value) {
  return String(value || 'simple-mark').replace(/[^a-z0-9_-]+/gi, '-').slice(0, 56) || 'simple-mark';
}

function channels(hex) {
  const value = Number.parseInt(hex.slice(1), 16);
  return [(value >> 16) & 255, (value >> 8) & 255, value & 255];
}

function mix(a, b, amount) {
  const aa = channels(a), bb = channels(b), t = Math.max(0, Math.min(1, Number(amount) || 0));
  return `#${aa.map((v, i) => Math.round(v + (bb[i] - v) * t).toString(16).padStart(2, '0')).join('')}`;
}

function rgba(hex, alpha) {
  const [r, g, b] = channels(hex);
  return `rgba(${r},${g},${b},${Math.max(0, Math.min(1, Number(alpha) || 0))})`;
}

function expressionPose(expression) {
  switch (expression) {
    case 'joy': return { yL: 51, yR: 51, gap: 13, sy: .64, rL: -2, rR: 2 };
    case 'calm': return { yL: 51.5, yR: 51.5, gap: 13, sy: .50, rL: 0, rR: 0 };
    case 'curious': return { yL: 51, yR: 49, gap: 13, sy: .82, rL: -3, rR: 4 };
    case 'mischief': return { yL: 50.5, yR: 51, gap: 13, sy: .70, rL: 7, rR: -7 };
    case 'sleepy': return { yL: 52, yR: 52, gap: 13, sy: .26, rL: 0, rR: 0 };
    case 'focused': return { yL: 50.5, yR: 50.5, gap: 11.5, sy: .68, rL: 5, rR: -5 };
    case 'determined': return { yL: 50.5, yR: 50.5, gap: 11, sy: .62, rL: 9, rR: -9 };
    default: return { yL: 50.5, yR: 50.5, gap: 13, sy: .82, rL: 0, rR: 0 };
  }
}

function eye(style, x, y, rotation, sy, ink, side) {
  const t = `translate(${x} ${y}) rotate(${rotation}) scale(1 ${sy})`;
  if (style === 'spark') {
    return `<path transform="${t}" d="M0-2.1.7-.7 2.1 0 .7.7 0 2.1-.7.7-2.1 0-.7-.7Z" fill="${ink}"/>`;
  }
  if (style === 'slit') {
    return `<path transform="${t}" d="M0-1.8 1.05 0 0 1.8-1.05 0Z" fill="${ink}"/>`;
  }
  if (style === 'closed') {
    const d = side === 'left' ? 'M-2 0Q0 1 2 0' : 'M-2 0Q0 1 2 0';
    return `<path transform="${t}" d="${d}" fill="none" stroke="${ink}" stroke-width="1.15" stroke-linecap="round"/>`;
  }
  return `<circle transform="${t}" r="1.35" fill="${ink}"/>`;
}

function eyesMarkup(style, expression, ink, eyeY = 50.5) {
  const p = expressionPose(expression);
  const offset = eyeY - 50.5;
  if (style === 'visor') {
    const y = (p.yL + p.yR) / 2 + offset;
    const tilt = expression === 'mischief' ? -4 : expression === 'curious' ? -2 : 0;
    return `<path d="M41 ${y}L55 ${y}" transform="rotate(${tilt} 48 ${y})" fill="none" stroke="${ink}" stroke-width="1.35" stroke-linecap="round"/>`;
  }
  const leftX = 48 - p.gap / 2, rightX = 48 + p.gap / 2;
  return `${eye(style, leftX, p.yL + offset, p.rL, p.sy, ink, 'left')}${eye(style, rightX, p.yR + offset, p.rR, p.sy, ink, 'right')}`;
}

const SHAPES = Object.freeze({
  classic_flame: {
    eyeY: 51,
    body: (fill) => `<path d="M48 8C46 20 37 27 37 39C31 37 26 43 27 53C28 68 37 81 48 88C59 81 68 68 69 53C70 43 65 37 59 40C59 28 52 20 48 8Z" fill="${fill}"/>`,
    accent: (hot) => `<path d="M48 22C46 31 42 37 42 45C45 43 48 42 51 43C54 36 52 29 48 22Z" fill="${hot}" opacity=".42"/>`,
    spark: () => '',
  },
  ember_orb: {
    eyeY: 50,
    body: (fill) => `<path d="M49 16C64 14 76 25 78 40C81 57 71 73 56 80C40 87 24 79 19 64C14 50 18 34 30 24C35 20 42 17 49 16Z" fill="${fill}"/>`,
    accent: (hot) => `<path d="M27 33C33 24 43 20 53 21" fill="none" stroke="${hot}" stroke-width="2.2" stroke-linecap="round" opacity=".38"/>`,
    spark: () => '',
  },
  shard_flame: {
    eyeY: 52,
    body: (fill) => `<path d="M49 7 70 29 58 40 68 55 48 89 46 64 27 55 38 40 29 28Z" fill="${fill}"/>`,
    accent: (hot) => `<path d="M48 20 58 31 49 40 54 52" fill="none" stroke="${hot}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" opacity=".38"/>`,
    spark: () => '',
  },
  split_flame: {
    eyeY: 57,
    body: (fill, shade) => `<path d="M47 88C35 83 28 73 29 60C30 49 38 45 39 35C40 28 37 21 35 17C44 20 48 28 48 39L48 51C43 50 40 55 41 62C42 72 45 81 47 88Z" fill="${fill}"/><path d="M49 88C61 83 68 73 67 60C66 49 58 45 57 35C56 28 59 20 62 15C53 20 48 28 48 39L48 51C53 50 56 55 55 62C54 72 51 81 49 88Z" fill="${shade}"/>`,
    accent: (hot) => `<path d="M48 55V76" fill="none" stroke="${hot}" stroke-width="1.6" stroke-linecap="round" opacity=".3"/>`,
    spark: () => '',
  },
  halo_core: {
    eyeY: 50,
    body: (fill, shade) => `<path d="M48 18 61 34 59 65 48 82 36 65 35 36Z" fill="${fill}"/><path d="M48 18 61 34 53 48 48 82Z" fill="${shade}" opacity=".5"/>`,
    accent: (hot) => `<path d="M20 50C25 31 40 24 57 28C70 31 78 41 79 53" fill="none" stroke="${hot}" stroke-width="3" stroke-linecap="round" opacity=".65"/>`,
    spark: () => '',
  },
  smoke_wisp: {
    eyeY: 54,
    body: (fill) => `<path d="M23 79C32 68 43 65 57 62C72 59 78 50 74 39C71 30 63 25 58 19C60 29 56 35 47 40C37 46 30 51 30 60C30 68 34 73 40 76C34 78 28 79 23 79Z" fill="${fill}"/>`,
    accent: (hot) => `<path d="M37 57C45 51 56 50 66 46" fill="none" stroke="${hot}" stroke-width="2" stroke-linecap="round" opacity=".34"/>`,
    spark: () => '',
  },
});

export function renderSimpleMark(familyId, label, options = {}) {
  const shape = SHAPES[familyId];
  if (!shape) throw new RangeError(`Unknown simple mark family: ${familyId}`);
  const color = safeColor(options.color);
  const fiery = options.fiery !== false;
  const eyeStyle = EYE_STYLES.has(options.eyeStyle) ? options.eyeStyle : 'round';
  const expression = EXPRESSIONS.has(options.expression) ? options.expression : 'bright';
  const uid = safeUid(options.uid);
  const deep = mix(color, '#24181a', fiery ? .54 : .64);
  const shade = mix(color, '#8f3428', .42);
  const hot = mix(color, '#ffd27a', fiery ? .64 : .38);
  const fill = fiery ? color : mix(color, '#6f6865', .22);
  const ink = rgba(deep, fiery ? .62 : .52);
  const body = shape.body(fill, shade);
  const accent = shape.accent(hot);
  const sparks = fiery ? shape.spark(hot) : '';
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" role="img" aria-label="${label}" data-family="${familyId}" data-expression="${expression}" data-eye-style="${eyeStyle}" data-fiery="${fiery}" data-uid="${uid}">
    ${body}
    ${accent}
    ${sparks}
    <g class="simple-eye-detail" opacity="${fiery ? .82 : .70}">${eyesMarkup(eyeStyle, expression, ink, shape.eyeY)}</g>
  </svg>`;
}

export const SIMPLE_MARK_EYE_STYLES = EYE_STYLES;
export const SIMPLE_MARK_EXPRESSIONS = EXPRESSIONS;
