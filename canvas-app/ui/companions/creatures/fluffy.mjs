import manifest from './assets/fluffy/manifest.mjs';
import { resolveManifest } from './resolve-manifest.mjs';
export const FLUFFY = resolveManifest(manifest, import.meta.url);
