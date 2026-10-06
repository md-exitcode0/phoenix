import { renderSimpleMark } from './simple_mark.mjs';

export const id = 'split_flame';
export const label = 'Split Flame';
export const metadata = Object.freeze({ id, label });
export const render = (options = {}) => renderSimpleMark(id, label, options);
export default Object.freeze({ id, label, metadata, render });
