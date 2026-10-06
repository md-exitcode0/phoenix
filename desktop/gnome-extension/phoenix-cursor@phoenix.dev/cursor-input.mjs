// Validate the complete batch before focus or input changes. Match the Rust
// caller's 25-action cap; reject rather than silently coerce or truncate.
export const KEYSYMS = {
    enter: 0xff0d, return: 0xff0d, tab: 0xff09, space: 0x20, plus: 0x2b,
    escape: 0xff1b, esc: 0xff1b, backspace: 0xff08, delete: 0xffff,
    home: 0xff50, end: 0xff57, pageup: 0xff55, pagedown: 0xff56,
    up: 0xff52, down: 0xff54, left: 0xff51, right: 0xff53,
    f1: 0xffbe, f2: 0xffbf, f3: 0xffc0, f4: 0xffc1, f5: 0xffc2, f6: 0xffc3,
    f7: 0xffc4, f8: 0xffc5, f9: 0xffc6, f10: 0xffc7, f11: 0xffc8, f12: 0xffc9,
    numenter:0xff8d, numpadenter:0xff8d, kp_enter:0xff8d,
    decimal:0xffae, numdecimal:0xffae, numpaddecimal:0xffae, kp_decimal:0xffae,
    numplus:0xffab, numpadadd:0xffab, kp_add:0xffab,
    numminus:0xffad, numpadsubtract:0xffad, kp_subtract:0xffad,
};
for(let digit=0;digit<=9;digit++) {
    for(const prefix of ['num','numpad','kp_'])KEYSYMS[`${prefix}${digit}`]=0xffb0+digit;
}
export function keypadKeycode(keyval) {
    const digits=[82,79,80,81,75,76,77,71,72,73];
    if(keyval>=0xffb0&&keyval<=0xffb9)return digits[keyval-0xffb0];
    return ({[0xffae]:83,[0xff8d]:96,[0xffab]:78,[0xffad]:74})[keyval] ?? null;
}
const MODIFIERS = {ctrl:0xffe3, control:0xffe3, shift:0xffe1, alt:0xffe9, super:0xffeb, meta:0xffeb};
export function parseKeyCombo(combo) {
    if (typeof combo !== 'string' || !combo.trim()) throw new Error('combo must be a non-empty string');
    const parts = combo.split('+').map(part => part.trim());
    const mods = [];
    let main = null;
    for (const part of parts) {
        if (!part) throw new Error('empty key component; use plus for the + key');
        const lower = part.toLowerCase();
        if (Object.hasOwn(MODIFIERS, lower)) {
            const key = MODIFIERS[lower];
            if (mods.includes(key)) throw new Error('duplicate modifier');
            mods.push(key);
        } else {
            if (main !== null) throw new Error('combo must contain only one main key');
            if (Object.hasOwn(KEYSYMS, lower)) main = KEYSYMS[lower];
            else {
                const chars = [...part], cp = part.codePointAt(0);
                if (chars.length !== 1 || cp < 0x20 || (cp >= 0x7f && cp <= 0x9f) || (cp >= 0xd800 && cp <= 0xdfff))
                    throw new Error(`unknown key: ${part}`);
                main = cp < 0x80 ? cp : 0x01000000 + cp;
            }
        }
    }
    // Modifier-only chords remain valid; tap the last modifier.
    if (main === null) main = mods.pop();
    return {mods, main};
}

export function validateWindowActions(actions) {
    if (!Array.isArray(actions) || actions.length < 1 || actions.length > 25)
        throw new Error('actions must contain 1–25 items');
    for (const [index, action] of actions.entries()) {
        const fail = message => { throw new Error(`action #${index + 1}: ${message}`); };
        if (!action || typeof action !== 'object' || Array.isArray(action)) fail('expected an action object');
        if (!['move','click','double_click','scroll','type','key','wait','drag'].includes(action.type)) fail('unsupported action type');
        const fields = {
            double_click:['type','x','y','button'], move:['type','x','y'], click:['type','x','y','button','double'], scroll:['type','x','y','dx','dy'],
            drag:['type','from_x','from_y','to_x','to_y','duration_ms'],
            type:['type','text'], key:['type','combo'], wait:['type','ms'],
        }[action.type];
        for (const field of Object.keys(action))
            if (!fields.includes(field)) fail(`unsupported field ${field}`);
        const integer = (key, min, max, optional = false) => {
            if (optional && action[key] === undefined) return;
            if (!Number.isInteger(action[key]) || action[key] < min || action[key] > max)
                fail(`${key} must be an integer from ${min} to ${max}`);
        };
        if (action.type === 'move' || action.type === 'click' || action.type === 'double_click' || action.type === 'scroll') {
            integer('x',0,2147483647); integer('y',0,2147483647);
        }
        if (action.type === 'click' || action.type === 'double_click') {
            if (action.button !== undefined && !['left','middle','right'].includes(action.button)) fail('unsupported mouse button');
            if (action.double !== undefined && typeof action.double !== 'boolean') fail('double must be boolean');
        }
        if (action.type === 'scroll') {
            integer('dx',-30,30,true); integer('dy',-30,30,true);
            if (action.dx === undefined && action.dy === undefined) fail('scroll requires dx or dy');
        }
        if (action.type === 'drag') {
            for (const field of ['from_x','from_y','to_x','to_y']) integer(field,0,2147483647);
            integer('duration_ms',0,10000,true);
        }
        if (action.type === 'wait') integer('ms',0,10000);
        if (action.type === 'type' && typeof action.text !== 'string') fail('text must be a string');
        if (action.type === 'key') {
            try { parseKeyCombo(action.combo); }
            catch (error) { fail(error.message); }
        }
    }
    return actions.map(action => action.type === 'double_click'
        ? {...action, type:'click', double:true}
        : action);
}
