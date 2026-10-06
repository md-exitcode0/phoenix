export function resolveManifest(manifest, base) {
  return Object.freeze({ ...manifest, variants: Object.fromEntries(Object.entries(manifest.variants).map(([variant, states]) => [variant, Object.fromEntries(Object.entries(states).map(([state, clip]) => [state, { ...clip, url: new URL(clip.url, base).href }]))])) });
}
