# Motion graphics (`motion_graphics` + bundled motionmaxxing)

Every Phoenix coworker can make motion graphics: launch films, promos, feature
reveals, brand stings, logo animations, kinetic type, social ads, UI demo
videos, animated heroes, and loading/intro/transition animations.

## Pieces

| Piece | Where |
|---|---|
| Vendored skill (Apache-2.0, upstream rev `8c8ec0f2`) | `vendor/motionmaxxing/` (`UPSTREAM.json` lists file hashes and what was left out) |
| Licenses | `vendor/motionmaxxing/{LICENSE,NOTICE}`, `licenses/motionmaxxing/`, `THIRD_PARTY_NOTICES.md` |
| Workflow tool | `src/tools/motion_graphics.rs`, registered in `KNOWN_TOOLS` (so every coworker, the chief of staff, and custom coworkers get it) with its schema in `src/tools/descriptions.rs` |
| Prompt trigger | "Motion gate" paragraph + capability line in `src/runtime/shared_contract.rs` (appended to every agent prompt) |
| Installed skill | `~/.phoenix/skills/motionmaxxing/`, written from the binary the first time the tool runs; refreshed when Phoenix ships a new bundle, except `taste/` (the user's verdicts). A copy without `.phoenix-bundle` is treated as user-managed and never touched. |
| Shell isolation | `src/tools/bash.rs` mounts `~/.phoenix/skills` read-only inside the Workspace Bubblewrap sandbox so skill scripts can run there |

## Actions

`guide` (SKILL.md + Phoenix adapter; `file` loads a reference such as `motion`, `idea`, `world`, `ui-demo`) ·
`check` (Node 22+, Chrome/Chromium, ffmpeg+ffprobe, Python 3.9+, optional ElevenLabs/Codex; never fails) ·
`start` (template + runtime into `film/`) · `still` · `review` (draft render → look.py → lint.mjs: G0/G2/G3/G5) ·
`render` (final; audio, shutter, grain) · `look` · `lint` · `script` (any other skill script with `args`).

Scripts run through the normal bounded `bash` executor: same permission mode,
Workspace isolation, cancellation and 600 s ceiling. Long or WebGL films should
render in `from`/`to` ranges.

## Requirements

Node.js 22+, Google Chrome or Chromium (or `CHROME_PATH`), ffmpeg + ffprobe
(including the `drawtext` filter for review contact sheets),
Python 3.9+. Optional: `ELEVENLABS_API_KEY` for voice/SFX/music, Codex CLI with
`image_generation` for surface plates. In Workspace mode the Bubblewrap sandbox
clears the environment and hides `$HOME`, so ElevenLabs keys from the
environment or `~/.elevenlabs` are only seen in Full Access (or with
`permissions.shell_isolation = "off"`).
