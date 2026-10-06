# Iris: managed reference-driven design

Iris uses the vendored TasteCode Design engine for actual website/interface builds. There is no Design-mode toggle. Ordinary chat, explanation, read-only review and Plan interactions do not start the controller. The short Iris role is host identity and scope; the current upstream phase prompt supplies design instructions. The previous custom plain-language/visitor mandate and generic aesthetic handbook are not part of this managed path.

## Source and adaptation boundary

Upstream: `Leonxlnx/tastecode`, pinned at `3ee7948d8ec9d3f2ac538c7ac9b6c9fa8e345c28`. `vendor/tastecode-design/UPSTREAM.json` identifies the original files and hashes. Vendored design-agent sources, phase prompts, reference images and validator algorithms are unchanged. `MANIFEST.json` records packaged executable/reference bytes. License, NOTICE and dependency notices remain in the package and legal/source locations.

`vendor/tastecode-design/dist/` contains transpiled upstream modules. `scripts/iris-design-runtime.mjs` adapts upstream orchestration to a bounded JSON protocol. `scripts/iris-design-preview.mjs` owns one instance of the original local preview runner. Host adapters supply Phoenix identity, actual model routing, permissions, persistence, cancellation and managed-browser transport; they do not replace visual instructions with a simplified checklist.

## Execution

The controller advances Brief → Brand → Page → Assets → Build → Preview → Review → bounded Repair. Each phase receives its upstream prompt and relevant approved artifacts. Selected reference images and their hashes remain fixed through the run. New-brand typography uses the original persisted draw. Page plans bind sections to concrete references and measured compositions. Acquired assets, exact-file requirements and source-quality checks use the original validators.

Internal phase JSON is consumed before ordinary final-answer handling; it is not a new authored user prompt or a completed website. Current phase images are attached as real model image inputs, not filenames or claimed observations. Providers that cannot inspect required images fail explicitly.

Iris retains normal working tools. Legacy `design_reference`/`design_studio` and overlapping skill-loading paths are excluded from this managed workflow. Phase scope limits editing before Build/Assets; permissions, explicit limits and cancellation still apply. Upstream completion at the repair limit is not a passed review.

## Preview and verification

The controller owns the preview service lifetime and reuses Iris's managed native browser. Exact planned CSS viewports are captured, including the document below the fold, with upstream settling and DOM-audit scripts executed in an isolated JavaScript world. The native capture preserves the requested width even when the page overflows. Capture height is capped at the upstream 12,000-pixel boundary; PNG dimensions and byte budgets are checked. Captures and DOM failures are passed to the original review/repair loop.

The host image-input path preserves the full raster for those registered PNG captures, including pages taller than the ordinary 8,192-pixel desktop limit. Its capture-specific decoder retains the 32 MiB file bound, a 12,000-pixel height bound, bounded allocation, and the aggregate image-message budget. Ordinary reference/desktop input limits are unchanged. This prevents a valid long-page capture from being rejected only when Review tries to see it.

Static previews use the upstream confined static server. Command-based previews currently require Full Access because the upstream package-script runner does not supply Phoenix's Workspace shell sandbox. A Workspace command preview must report that limitation rather than silently bypass the sandbox. Talk and Plan cannot start managed edits. Full Access is still not authorization to publish, contact people or spend on unrelated work.

A rendered screenshot is not proof of motion, factual claims or working interactions. Upstream Build instructions require relevant browser exercises; independent acceptance must inspect the actual resulting site. Source/transport tests and synthetic controller tests are not autonomous website-quality acceptance.

## State and installation

Native state is scoped to the exact conversation, workspace, browser instance and originating turn under the existing runtime state root's `iris_design/` directory. A workspace claim prevents another flow from restoring or advancing its artifacts concurrently. Interrupted uncommitted operations retain evidence and fail closed rather than silently redraw references or rerun ambiguous work.

The installer stages `scripts/iris-design-{runtime,preview}.mjs` plus the complete vendor tree as one checked unit in `~/.phoenix/design-runtime` (or the state root selected by `PHOENIX_HOME`), using the existing atomic install/rollback process. Run `./install.sh --design-runtime-only` to install or upgrade just that package without Cargo, binary installation, saved-role synchronization, template changes or GNOME-extension changes; the previous runtime is retained at `design-runtime.prev` on upgrade. This option requires the complete verified source package and cannot be combined with `--jobs` or a binary target directory. Running the installer without the option retains the full build/install behavior. Node.js 22 or newer is required. `PHOENIX_NODE` can name its executable; `PHOENIX_IRIS_DESIGN_ROOT` can explicitly select an installed runtime root. The development repository remains a supported fallback. A copied binary without its runtime package is not a complete installation.

A saved `~/.phoenix/prompts/frontend_system.md` overrides the compiled role. Migrate that overlay through the normal revisioned, backed-up writer; do not claim the new role is active from a build alone. `cargo run --offline --example inspect_iris` checks the resolved role without a provider request.

## Updates and evidence

Use the review-only update checker to inspect upstream changes. Do not overwrite active runs with a different manifest. Rebuild the package with `node scripts/iris-design-runtime-build.mjs` only after reviewing changes and notices, then rerun the focused upstream/host tests and native captures. Keep exact failed evidence.

Current implementation and acceptance receipts belong in `artifacts/iris-design-mode-2026-09-20/`. The previous selective four-action port and directly authored landing page remain historical, not evidence for this engine's autonomous outcome. The original old checkout is preserved unchanged.
