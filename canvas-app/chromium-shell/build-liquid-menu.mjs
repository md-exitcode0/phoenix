import { build } from "esbuild";

await build({
  entryPoints: ["liquid-plus-menu.jsx"],
  outfile: "../ui/liquid-plus-menu.js",
  bundle: true,
  minify: true,
  format: "iife",
  platform: "browser",
  target: "chrome120",
  define: { "process.env.NODE_ENV": JSON.stringify("production") },
});
