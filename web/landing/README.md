# Phoenix landing page

An actual, framework-free product site under `web/landing`, not a fictional
acceptance brief. Open through a local HTTP server. `app.js` uses only native
browser APIs; no remote font/CDN/analytics dependency or package installation.

The page explains Phoenix as a desktop application, shows actual interface
captures with explicitly example content, lets visitors explore/copy four task
briefs, inspect coworker roles, switch site/product themes, and read truthful
release and privacy information. No connected signup/download endpoint is faked.

Before public deployment, supply the chosen domain, approved release and platform
details, privacy/contact information and a real distribution/signup route. Review
all marketing claims against the release being offered. Deployment is not part of
this local build. Do not copy the test screenshot fixtures into live work history.

The appearance and functionality are not an autonomous Iris acceptance result:
the user paused model runs. They were implemented directly during engineering.
The integrated Iris tools are verified separately by native deterministic tests.

Assets are generated from the actual Phoenix renderer by the local review
fixture; they must retain that provenance and the adjacent example labels. Font
files remain local website dependencies and retain their license; do not offer
them as user-facing downloadable deliverables.
