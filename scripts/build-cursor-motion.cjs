'use strict';
// Pure frontend bundling with an already-installed esbuild; no package installs.
const path=require('node:path'),fs=require('node:fs');
const root=path.resolve(__dirname,'..');
const esbuild=require(process.env.PHOENIX_ESBUILD||'esbuild');
const code=esbuild.buildSync({entryPoints:[path.join(root,'canvas-app/chromium-shell/cursor-motion-entry.mjs')],bundle:true,write:false,format:'iife',globalName:'PhoenixCursorMotion',platform:'browser',target:'es2022',minify:true,legalComments:'inline'}).outputFiles[0].text;
const banner='/* Cua AI, Inc. MIT cursor motion algorithms. See vendor/cua-motion/LICENSE and provenance.json. */\n';
for(const file of ['canvas-app/chromium-shell/cursor-motion.js','review-shell/ui/cursor-motion.js'])fs.writeFileSync(path.join(root,file),banner+code);
