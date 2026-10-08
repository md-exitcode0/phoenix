# Third-party notices

## TasteCode design-agent

Phoenix includes the TasteCode Design engine and reference library from
`Leonxlnx/tastecode`, revision `3ee7948d8ec9d3f2ac538c7ac9b6c9fa8e345c28`,
with Phoenix host adapters. Upstream source hashes and packaged-file hashes are
recorded in `vendor/tastecode-design/UPSTREAM.json` and `MANIFEST.json`.

Copyright 2026 TasteCode contributors. The engine is licensed under Apache
License 2.0. Original licenses, NOTICE, dependency notices, and asset information
are retained in `vendor/tastecode-design/` and `licenses/tastecode/`.
See [docs/iris-design.md](docs/iris-design.md) for the integration boundary and
[the package notices](vendor/tastecode-design/THIRD_PARTY_NOTICES.md) for details.

## motionmaxxing motion-graphics skill

Phoenix bundles the motionmaxxing skill from `Tejashmakwana/motionmaxxing`,
revision `8c8ec0f2a6f6c9a0da15cd24f7b1298ab298368c`, unmodified except that the
showcase media (`docs/media/`, `docs/hero/`, `studies/*.jpg`) is left out. It
lives in `vendor/motionmaxxing/` (file hashes in `UPSTREAM.json`), is embedded
in the Phoenix binary, and is installed to `~/.phoenix/skills/motionmaxxing/`
the first time an agent calls the `motion_graphics` tool.

Copyright Tejas Makwana. The skill is licensed under Apache License 2.0. It
bundles GSAP 3.12.5 (GreenSock Standard "No Charge" License, not open source),
three.js r186 and opentype.js 1.3.4 (MIT), and the Inter font (SIL OFL 1.1);
each stays under its own license. The original LICENSE and NOTICE are retained
in `vendor/motionmaxxing/` and `licenses/motionmaxxing/`.

## Banana reference photograph

The reusable test input `artifacts/banana-reference/banana-reference.jpg` is
“Banana-Single.jpg” by Evan-Amos, from Wikimedia Commons, supplied under
CC BY-SA 3.0 without modification. Source and license links, attribution, and
integrity hash are retained in `artifacts/banana-reference/source.json`.

## T3 Code

Phoenix's Dev View uses the visual and interaction design of T3 Code.

MIT License

Copyright (c) 2026 T3 Tools Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## Unlazy

Phoenix's shared persistent-completion system prompt adapts the acceptance
ledger, multi-pass craftsmanship, re-verification, and final completion-audit
method published in the Unlazy agent skill. Phoenix uses its native durable
goal, work, todo, and evidence runtime instead of Unlazy's checker and hooks.

Source: https://github.com/Leonxlnx/unlazy

MIT License

Copyright (c) 2026 Leonxlnx

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## Ponytail

Phoenix's internal Lean Build Ladder adapts the software-engineering method
published in the Ponytail project. The project name is not used in Phoenix's
product interface or coworker prompts.

Source: https://github.com/DietrichGebert/ponytail

MIT License

Copyright (c) 2026 DietrichGebert

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## Devicon

Phoenix uses rasterized Devicon artwork for programming-language file icons.

Source: https://github.com/devicons/devicon

Devicon is released under the MIT License. Individual logo trademarks remain
the property of their respective owners.

## xterm.js

Phoenix uses xterm.js and its fit addon to render the desktop PTY terminal.

Source: https://github.com/xtermjs/xterm.js

MIT License

Copyright (c) 2017-2019, The xterm.js authors
Copyright (c) 2014-2016, SourceLair Private Company
Copyright (c) 2012-2013, Christopher Jeffrey

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
