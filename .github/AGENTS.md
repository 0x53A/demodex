# GitHub Pages PWA

pages.yml builds and deploys the static Rust/Yew PWA on pushes to main or manual
workflow dispatch (deployment only from main). In repository Settings → Pages,
set Build and deployment → Source to GitHub Actions before the first run.
The workflow does not change repository settings or require custom secrets.

The artifact contains only web/.pages-dist, never server runtime state, tokens,
VM images, or deployment backups. Nightly is pinned to the locally tested
2026-05-20 toolchain; Trunk is pinned to 0.21.14. The Linux runner uses rustup
directly, while local Nix builds retain the loader wrapper.

Build root-hosted instances as before: uv run tools/build-web.py inside shell.nix.
For a separate Pages client:

    uv run tools/build-web.py --standalone --public-url /demodex/ --dist web/.pages-dist

--standalone leaves the initial host field empty. configure-pages supplies the
deployment path, so the workflow also supports a root custom domain. Manifest,
icons, service worker, navigation and generated WASM assets remain under that
path. tests/pages.py checks actual loading, worker scope and offline launch.

The Pages PWA still connects directly to a user-entered HTTPS daemon. That
daemon must explicitly permit the PWA's origin, e.g.:

    --allowed-origin https://0x53a.github.io

Origins exclude paths (do not include /demodex/). Set services.demodex.allowedOrigins
for the NixOS module. No server allowlists are changed by this workflow.
Tailscale must still be connected for private Tailscale hosts; browser local
network permissions may also apply. Exact protocol/schema checks remain active.
The existing per-instance PWA deployments continue to work independently.

Saved connections/tokens belong to the browser origin. They are not transferred
from a per-instance PWA to Pages, or included in the published artifact.
