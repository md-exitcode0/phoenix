import { renderSimpleMark } from './simple_mark.mjs';

export const id = 'shard_flame';
export const label = 'Shard Flame';
export const metadata = Object.freeze({ id, label });
export const render = (options = {}) => renderSimpleMark(id, label, options);
export default Object.freeze({ id, label, metadata, render });
