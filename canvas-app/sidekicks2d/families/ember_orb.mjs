import { renderSimpleMark } from './simple_mark.mjs';

export const id = 'ember_orb';
export const label = 'Ember Orb';
export const metadata = Object.freeze({ id, label });
export const render = (options = {}) => renderSimpleMark(id, label, options);
export default Object.freeze({ id, label, metadata, render });
