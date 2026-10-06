# Phoenix host adapter for TasteCode Design mode

The design engine and reference library are imported from TasteCode commit
`3ee7948d8ec9d3f2ac538c7ac9b6c9fa8e345c28`. Every upstream source file, reference,
and legal notice is byte-identical to that revision. `UPSTREAM.json` records the
source hashes; the build checks them before emitting JavaScript. The previous
checkout at `artifacts/iris-design-integration-2026-09-18/upstream` is not used as
an update target.

`src/`, `references/`, `package.json`, `tsconfig.json`, `host-reference/`, and
the seven TypeScript files under `server/src/` and `server/proc/` are upstream
originals. `dist/` is an unbundled ESM translation with the original relative
imports and `import.meta.url` reference resolution. `server/dist/preview.js`
bundles the original preview lifecycle and its dependencies. The dependency
archives match the exact integrity values in upstream's lockfile; their original
license files and package metadata remain under `server/dependencies/`.

The Phoenix-specific code is in `scripts/iris-design-runtime.mjs` and
`scripts/iris-design-preview.mjs`. These host the upstream functions; they do not
introduce a design-mode toggle, a replacement design doctrine, an alternate
prompt set, or a provider implementation.

## Installation and trust

Keep this relative installation layout:

```text
INSTALL_ROOT/
  scripts/iris-design-runtime.mjs
  scripts/iris-design-preview.mjs
  vendor/tastecode-design/
    MANIFEST.json
    UPSTREAM.json
    src/ dist/ references/
    server/ host-reference/
    LICENSE NOTICE THIRD_PARTY_NOTICES.md
    package.json tsconfig.json ADAPTER.md
```

`MANIFEST.json` covers every vendored and compiled file, all reference images,
the preview dependencies, and both executable helpers. It is not a signature.
The native owner must install the tree outside actor-writable paths, pin the
manifest digest in trusted run metadata, and reject changed or missing package
files on start/resume. `check` performs a full verification. Each helper action
also verifies executable and metadata hashes before importing vendored code;
selected reference, supplied image, approved asset, and capture snapshots are
checked during the run. Full image-library hashing occurs at start and explicit
`check`, not on every model turn.

The native owner persists the returned opaque state outside the actor workspace.
It must bind that state to the real session, turn, workspace, and current state
revision; actor output must never replace trusted state fields. Approved `.taste`
JSON artifacts remain in the workspace as upstream intended. The host retains
trusted copies and restores a changed artifact before reporting its violation.

## Runtime protocol

Run `node scripts/iris-design-runtime.mjs`. Send one UTF-8 JSON object on stdin,
then close stdin. Exactly one JSON value and a newline are written on stdout.
The input and output are bounded to 8 MiB. A provider final response string is
bounded to 2 MB. The process makes no provider calls and starts no preview server.

```json
{"action":"check","expectedManifestSha256":"optional previously pinned digest"}
{"action":"start","request":"Build a website for Phoenix.","workspace":"/absolute/workspace","references":[]}
{"action":"context","state":{}}
{"action":"advance","state":{},"output":"provider final response"}
```

`check` returns `{ok,upstreamRevision,manifestSha256,files,verified}`. `start`
returns a raw state with `version:1`, an id, revision, pinned upstream and package
hash, request, canonical workspace, phase, status, and pre-run file/source
baselines. References must be existing absolute raster paths. An empty reference
array invokes the actual upstream library selection after a validated brief.
Explicit references keep the upstream behavior of taking precedence over the
internal reference deck. Upstream's optional user reference-library configuration
is preserved; a missing or invalid configured library produces an error.

`context` returns:

```text
{
  phase, status,
  kind: "model" | "capture" | "terminal",
  prompt: string | null,
  attachments: absolute image paths[],
  previewPlan?, previewUrl?,
  outcome?, accepted?, completion?, error?,
  repairAttempt
}
```

For `kind:model`, submit that prompt and those attachments to the selected native
actor, retaining the phase conversation so bounded correction prompts have their
original context. The prompt bodies come from the upstream engine. The selected
reference-workflow wrapper, progress-commentary prefix, and unavailable-assets
replan text come from the pinned upstream server orchestrator. The font draw and
reference selections persist in state and are not rerolled on corrections or
resume. `advance` accepts a provider final string or an equivalent JSON object
and returns the raw next state.

Phases are brief, brand, page, assets, build, preview, review, repair, and
complete. Planning validates the original schemas, generated palette and font
choice, page layout/reference choices, asset coverage and provenance. A missing
meaningful visual gets one upstream page replan. Brand/page/assets allow up to
three distinct schema corrections; a repeated diagnostic fails immediately.
Other phases allow one correction. Build and repair preserve exact deliverable
boundaries and the original source-quality gate. Pre-existing source violations
are tracked before any design phase can write files. Visual correction cycles
continue while review still has validated findings; the outer Phoenix working
horizon, explicit cancellation, permissions, and concrete blockers own stopping.
A corrected preview plan does not reset a failed native server start's retry
allowance.

Invalid transport, unsupported state/action, package-integrity problems and bad
capture inputs return exit code 1 with:

```json
{"error":{"code":"INVALID_INPUT","message":"specific diagnostic"}}
```

Model schema or phase validation failures return exit code 0 with a corrected or
failed next state. The caller must inspect `status`, `kind` and `outcome`, not
the process exit code alone. There is no fallback to the previous simplified
design tool.

## Preview and capture hooks

A validated preview plan returns `phase:preview`, `status:waiting`, and
`awaitingCapture:true`. The native owner starts the preview helper and performs
the real browser captures. For a server startup/plan failure, return:

```json
{"action":"preview_error","state":{},"reason":"actual startup failure"}
```

For success, submit exactly one capture for every unique planned viewport:

```json
{
  "action":"capture",
  "state":{},
  "previewUrl":"http://127.0.0.1:4173/",
  "screenshots":[{
    "path":"/trusted/captures/desktop.png",
    "width":1440,
    "height":1000,
    "domAudit":{
      "h1Count":1,
      "interactiveTargetViolations":[
        {"selector":"button.small","label":"Open","width":20,"height":20}
      ]
    }
  }]
}
```

The screenshot paths must be unique and absolute. Width and height describe the
requested CSS viewport, not necessarily the full-page raster. Native fields such
as `captureHeight` and `fullDocument` are permitted and ignored by the original
review schema. Captures are frozen by SHA-256. The host checks file integrity;
the native collector is responsible for provenance, actual viewport selection,
settling, and collecting DOM measurements from the same rendered page. Tests use
explicitly synthetic fixtures and are not evidence of agent visual quality.

The original asset raster validator has a 16 MP cap. Native RGB/RGBA PNG captures
use a separate streamed checksum/decode gate accommodating upstream full-page
capture sizes: 32 MB input, 48 MP raster, 200 MB decoded data processed without
retaining the full decoded raster. Reference and asset limits are unchanged.
Other raster encodings retain the upstream limits. A native DOM audit can force
repair even if the model claims a pass. Audit fields are optional in the upstream
schema; the native integration should always supply measurements when available.

If capture really cannot be performed, explicitly submit
`{action:"capture_unavailable",state,reason}`. The terminal outcome is
`review_skipped`, never accepted. Successful repair requests fresh captures using
the same preview plan before another review can occur.

## Owned preview service

Run `node scripts/iris-design-preview.mjs` with stdin kept open. Send a single
newline-terminated JSON start request:

```json
{"workspace":"/absolute/workspace","plan":{},"expectedManifestSha256":"optional pinned digest"}
```

The service checks the full package, validates the original `PreviewPlan`, and
calls upstream `startDesignPreview`. Success emits
`{ready:true,url,viewports}`. The final URL may use another port because the
original runner avoids occupied ports. Failure emits
`{ready:false,error:{code,message}}` and exits nonzero. No child stdout or stderr
is forwarded into this protocol. Upstream retains a bounded diagnostic buffer;
an error message is limited to 100,000 characters. There is only one start per
service. Close stdin or send SIGTERM/SIGINT to stop its owned server.

The original static server retains public-path/credential boundaries, MIME
checks, resource validation, ownership headers and loopback binding. Command
preview retains upstream's package-script/workspace validation and reduced
environment; it cannot run npx or a model-selected network package.

The host wrapper tracks the actual detached spawn handles solely to accelerate
EOF/SIGTERM cancellation while upstream is awaiting readiness. A confirmed
signal exit during cancellation is exposed as a conventional nonzero 128+signal
status because the pinned readiness loop checks `exitCode` but ignores Node's
`signalCode`. Vendored source is unmodified. Upstream still performs its own
process-group cleanup. A raw SIGKILL cannot execute JavaScript cleanup: native
owners must allow graceful drain or retain external ownership of child groups.
Windows process-tree behavior remains upstream and requires a Windows test;
the deterministic lifecycle evidence here is from Linux.

## Completion semantics

`status:complete` alone does not mean quality acceptance:

| Outcome | Accepted | Meaning |
| --- | --- | --- |
| passed | true | Upstream review passed after zero or more visual correction cycles. |
| review_skipped | false | Native capture could not be performed. |
| not_design | false | Briefing returned its explicit non-design result. |

`status:failed` preserves the exact phase and diagnostic. Provider quota,
cancellation and native tool errors remain the native runtime's responsibility;
they must not be supplied as invented successful phase output.

## Rebuild, verify and update

Node 22 or later is required. Existing esbuild is used only during rebuild;
there are no install-time or runtime npm commands. The default build dependency
is `canvas-app/chromium-shell/node_modules/esbuild`; `IRIS_ESBUILD_PATH` can point
to an existing installation.

```text
node scripts/iris-design-runtime-build.mjs
node --test scripts/iris-design-runtime.test.mjs scripts/iris-design-runtime-preview.test.mjs
```

For an upstream update, first inspect the complete new revision and compare the
design package and server orchestration/lifecycle. Preserve the old source and
failed evidence. Use a NEW clean checkout at the reviewed full commit SHA and
explicitly move the old vendor tree aside. Update the pinned revision constants
and reviewed dependency integrity values in the host scripts where necessary.
Import originals with `scripts/iris-design-runtime-update.mjs NEW_CHECKOUT SHA`,
then import preview files/dependency archives with
`scripts/iris-design-runtime-update.mjs --preview NEW_CHECKOUT ARCHIVE_DIRECTORY`.
The script verifies archive integrity and does not execute npm lifecycle scripts.
Reapply this host integration document, rebuild, and run the deterministic tests.
Approve the new manifest externally before installing it. Runs pinned to an old
manifest must not silently continue under a different engine.

Between d567c74 and the pinned revision, design-agent changes only add capability
probing around symlink-dependent tests for systems without symlink privileges.
Production design algorithms, prompts and reference assets did not change.
The server orchestrator additionally delays clearing correction flags until
artifact writes, asset snapshots, and repair validation really succeed; the
adapter preserves that corrected ordering. Its other changes route durable
queued Side-chat events, which belong to the native host rather than this design
adapter. The imported desktop PATH helper also adds Windows' bundled Codex path.
