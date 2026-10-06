import { renderSimpleMark } from './simple_mark.mjs';

export const id = 'halo_core';
export const label = 'Halo Core';
export const metadata = Object.freeze({ id, label });
export const render = (options = {}) => renderSimpleMark(id, label, options);
export default Object.freeze({ id, label, metadata, render });
