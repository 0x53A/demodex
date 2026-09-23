# Demodex

Rust manager and mobile-first Rust/Yew PWA for Codex sessions and isolated NixOS
work VMs. Instances are independent; the browser connects directly to a selected
daemon. There is no central coordinator. See `DESIGN.md` for the UI design system.

## Code and transport

- `src/`: core library, daemon, orchestration, persistence and VM management.
- `src/service.rs`: transport-independent operation dispatch and durable receipts.
- `src/wormhole.rs`: authenticated actors; `src/http.rs`: static assets and upgrades.
- `crates/client/`: reusable native Wormhole client, used by Frosticus and the CLI.
- `crates/web/`: the maintained Yew HTML/WASM frontend. No Node/pnpm toolchain is
  needed. Small JavaScript files provide PWA startup and service-worker glue.
- `crates/protocol/`: shared actor API and compatibility contract.
- `nix/host-service.nix`: NixOS `services.demodex` module.
- `tools/build-web.py`: frontend builds; `tests/`: Rust-adjacent integration and
  browser checks, run with UV.

Use Codex app-server JSON-RPC, never terminal scraping. The compatibility
baseline is codex-cli 0.154.0; this is an experimental API. Preserve unknown
events for inspection and report unsupported requests explicitly.

The PWA uses ractor_wormhole at `/wormhole`. A JSON hello checks protocol VERSION
and wire-schema fingerprint before the Wormhole handshake or authentication.
Both peers reject mismatches. Keep wire declarations in
`crates/protocol/src/wire.rs`: build.rs hashes normalized declarations and locked
transport dependencies. Do not move wire types into unhashed modules or introduce
target-specific shapes. The fingerprint is not Rust TypeId or proof of semantic
compatibility. Bump VERSION for semantic changes, including JSON payloads;
`crates/protocol/src/lib.rs` is the source of truth for the version.

After login, typed actor calls and pushed change notices carry UI traffic. The
frontend coalesces notices and fetches snapshots and event pages by cursor;
periodic server notices refresh account/runtime state. Render through Yew's
Message/update/view model. There is no REST API or Axum dependency. Wormhole calls
the core Service directly with shared typed operations and responses. Diagnostics
use `demodex call`; library and native-client integration are described below.

Storage and service dispatch reuse the shared session/event records directly,
without JSON round-trips between duplicate types. Extensible upstream Codex
records retain serde_json::Value; the browser converts typed replies to its dynamic
view model. Forms construct shared command types directly. Invalid saved sandbox
choices produce validation messages, never silently select default access.
Data-record serde encoding lives alongside the wire declarations; encoding changes
require a version bump because the structural hash excludes implementations.
Exact transport dependency versions constrain consumers with separate lockfiles.

## Library and native clients

`demodex::Runtime::start(Config).await` opens storage, starts the configured
executor and owns periodic monitoring. Obtain cloneable handles with
`runtime.service()`. `Service::call(request_id, Operation)` is a trusted in-process
API: authorization belongs to its host application.

Await `runtime.shutdown()` before leaving Tokio: it rejects new calls, drains
admitted operations and stops executors. Drop requests asynchronous cleanup,
which requires a live Tokio runtime. Every Service handle retains the private
state-directory lock, including closed handles; release them before reopening
the same directory. Individual browser connections do not own the runtime.
Connection-scoped actors and socket readers are cancelled together; accepted
commands finish and record receipts even after their caller disconnects.

`demodex-client::Client` opens a native WebSocket, checks Hello before credentials,
authenticates and subscribes to notices. `call` takes an explicit receipt ID;
`read` refuses mutations. It never reconnects or retries calls automatically.
`subscribe()` reports changes and connection closure. Dropping the client releases
its socket reader and actors. Calls return uncertainty promptly on connection
closure rather than waiting out the RPC deadline. Native TLS uses system roots.

`demodex call --url ws://127.0.0.1:4780/wormhole --token-file PATH` reads JSON lines
from stdin and prints one Result per line. Example read:
`{"operation":"Sessions"}`. Mutations should supply a stable UUID `request_id`;
the CLI generates one when omitted for interactive/test convenience. Keep IDs to
inspect uncertain results with `{"operation":{"Receipt":{"id":"UUID"}}}`.
Logs go to stderr; neither URLs nor arguments contain access tokens.

Frosticus lives at ../frost/codicillus-frosticus and depends on the client/protocol
crates by local path. Its separate member Wormhole protocol enforces ownership;
it does not expose Demodex's administrative actor to browsers. Rebuild affected
clients and daemons together after incompatible protocol changes.

## Session and execution contracts

Conversation identity, agent runtime, execution target and browser lifetime are
separate. Browser portals own no sessions. The daemon owns Codex RPC connections,
executors and approvals across browser disconnects and static-service restarts.

Persist UUID mutation receipts before execution. Duplicate requests return the
recorded result; uncertain commands, prompts and approvals must never be replayed
automatically. Replacing an executor invalidates its process/file handles and
assigns a fresh executor identity. Never route old handles into a replacement.
Requests from a lost RPC generation remain visibly unavailable. Unsupported
approval types use explicit JSON response editing, not an automatic answer.
Questions and approvals displayed by Demodex remain pending until the user
responds; browser timeouts and reconnects must not submit a default answer.

SQLite retains the existing schema. New receipts store a versioned typed reply;
old string-encoded replies are decoded only on the legacy receipt path. v14
JSON-string inputs are normalized only for command identity comparison. Completed
receipts return their recorded result; interrupted receipts remain uncertain and
never replay. Receipt queries expose decoded data for older persisted records too.

Host-session creation/import accepts an existing absolute working directory.
Blank preserves the service default or imported directory; an explicit path
overrides it for a new attachment. Importing again cannot retarget an existing
attachment. Display the active executor directory rather than relying on a
resumed thread's historical top-level cwd.

Persist sandbox choices separately from the user's Codex configuration:
read-only, workspace-write, danger-full-access, or no override. Creation/resume
sends `sandbox`; idle changes use `thread/settings/update` with `sandboxPolicy`.
Reject changes during active turns on both server and UI. Show the accepted
effective policy; approval policy stays on-request. No override on a connected
thread retains its current policy. Preserve existing SQLite data during changes.

Saved threads use `thread/list` with search and pagination. Selecting one fills
an explicit resume form. Stop its CLI controller before attaching; resume restores
persisted history, not live CLI processes or shell jobs. Empty threads may lack a
persisted rollout: expose `no rollout found` rather than silently replacing them.

## Shared execution targets

Targets are daemon-scoped. The registry includes the configured native host,
managed VMs, and named external executors. Several sessions may select the same
VM; sessions do not own or exclusively reserve targets. VM provisioning is also
available in host mode. Host execution still requires `--host-workspace`; never
silently expose the host from an isolated default deployment.

`execution_targets` stores external endpoints; `session_targets` stores ordered
stable target selections and per-session working directories. Keep these IDs
separate from Codex executor generations in `sessions.targets`. Migrate legacy
attachments without deleting history or overriding an existing selection.
`runtime_sessions` identifies sessions created directly against the daemon runtime.
`host_sessions` and `session_environment` identify legacy managed runtime
attachments, not the current set of tool targets. VM stop/loss disconnects all
current users. Reconnect resolves the saved selection and preserves the thread.

Changing targets requires a connected, idle session, no unresolved decisions,
no queued messages, and a paused/inactive goal. Register and validate executors
before persisting the selection. The current app-server API applies environments
on the next `turn/start`, not through `thread/settings/update`; show that pending
state and block goal/queue resumption until an explicit message applies it.
Never send a hidden model turn to apply settings. The first selected target is
the primary image-upload destination; unsupported external uploads fail without
host fallback. Empty selection disables execution targets explicitly.

SSH targets are configured in Server settings and persisted in `ssh_targets`.
The adapter runs locally and speaks the Codex executor protocol. Foreground
commands use standard OpenSSH; file operations use the server's existing SFTP v3
subsystem. No Python, remote helper executable or custom server is installed.
Linux, a POSIX-compatible login shell, sh, env (including `env -0`) and cat are
required remotely. Credentials and known_hosts belong to the daemon user.
Host-key verification and batch authentication are mandatory. The app-server
must share the daemon's network namespace. The adapter rejects browser Origins
and checks the connecting socket's Linux UID.

`process/start` waits for completion while streaming output, with no command
runtime limit. The protocol has no command deadline field. Agents may explicitly
use a remote timeout utility when they want a deadline; never infer one from
yield/poll intervals. Completed process IDs are receipts, not background handles. PTYs, stdin
pipes/writes, custom argv0, remote signals and process resumption are unsupported.
`process/terminate` cancels the local SSH invocation; remote termination is best
effort. Transport failure, cancellation and status 255 have uncertain remote
outcomes, never an invented exit code. Never replay uncertain commands.
Independent foreground SSH requests can run concurrently. Agent instructions
and session-context results explain that agents may check for remote tmux and
manage named tmux jobs through ordinary commands for background work. Demodex
does not install tmux or track/clean up those remote jobs.

SFTP supports bounded whole-file reads/writes (32 MiB), metadata, canonical paths,
directory listings, single-directory creation and non-recursive removal. File
stream handles, strict no-follow guarantees, recursive operations, copy and walk
are explicitly unsupported; agents can use foreground commands as appropriate.
Image uploads create a private remote directory and file via SFTP. Restricted
sandbox policies, managed networking, shell snapshots, capability discovery,
environment-config reads and HTTP proxying remain unsupported. SSH commands use
the remote account's authority and require danger-full-access. Replacement
requires paused attached sessions, disconnects them and creates a fresh executor
identity. Saved target configuration remains compatible with the earlier adapter.
The managed-VM backend remains separate: it runs remote `codex exec-server`.
Run `uv run tests/ssh.py` in the host shell for disposable SSH keys/sshd, browser
settings, executor protocol and real Codex attachment checks without inference.

## Authentication and isolation

The manager binds loopback by default and creates an owner-only random token in
its data directory. Browser and native actor login carry the token after schema
negotiation. Never put tokens in URLs. There is no anonymous session API.

Optional `--tailscale-user` entries enable `tailscale.sock`, mode 0600 in the
private data directory. Only this socket trusts a single exact
`Tailscale-User-Login` header, supplied by Tailscale Serve, plus an explicitly
allowed browser Origin. Direct TCP ignores identity headers and requires an actor
login token. Empty login tokens may use trusted identity; explicitly supplied
invalid tokens must fail even if identity is valid. The service owner and root
are within this trust boundary. Keep the Unix proxy's root-path alias off the
TCP router so integrated static serving continues to work.

Cross-origin frontends need exact `--allowed-origin` entries, never wildcard
access. Origins contain scheme, host and nondefault port, not paths; allowing a
Pages origin trusts all applications at that origin. Protocol compatibility,
Tailscale connectivity and browser local-network permissions still apply.

Managed VM mode uses one persistent bubblewrap app-server with a dedicated
CODEX_HOME/login shared by conversations. Its filesystem excludes host home,
project mounts, SSH credentials and Docker sockets; the host Nix store is
read-only and networking is shared. Never copy desktop authentication stores.

Native host mode starts app-server and executor as the service user and carries
that user's normal filesystem, device and sudo authority; it is not host
isolation. `--host-workspace /absolute/workspace` selects this mode. The default
is a dedicated profile; `--codex-home /absolute/profile` explicitly reuses a CLI
profile without copying credentials or writing default configuration into it.
The module equivalent is `services.demodex.codexHome`; null selects a dedicated
profile. Do not assume separate processes sharing a profile coordinate refresh.

Dedicated login uses `chatgptDeviceCode`: show its verification URL and code,
allow completion on another device, clear the code after account detection, and
require another explicit attempt after expiry. Device authorization must be
permitted by the account. No localhost callback forwarding is needed.

External `ws://` or `unix://` app-servers and `ws://` executors are supported;
their isolation is the operator's responsibility.

## Browser state and images

Saved connection names, normalized URLs and tokens persist in localStorage,
scoped to the frontend origin. Fresh same-origin windows try identity or a saved
token; standalone builds start with the connection picker. Successful login
updates the saved entry. Forget removes its saved token and disconnects the
current local client; other open tabs may retain authenticated connections.
Active-tab state and drafts remain tab-scoped and host-specific.

Browser history stores navigation only, never credentials or drafts. Back/forward
restores screens; history for another host returns to the connection picker.
Reload replaces the current history entry, and navigation must not trap the user
at the app root or submit commands. Keep fixed header/composer, independent
transcript scrolling, and conditional following that respects scrolling upward.

The service worker caches verified static assets only, never API responses,
credentials or conversations. Preserve drafts across updates. Do not force
reloads of other open tabs or replay mutations on reconnect. There is no offline
conversation cache or browser/OS push notification service.

Image picker and clipboard uploads use authenticated actor calls and mutation
receipts. Validate supported signatures (PNG/JPEG/GIF/WebP) and the 4 MiB limit;
filenames/MIME labels are untrusted and signature checks are not image decoding.
Store host files privately in the data directory; VM uploads use its existing
SSH identity and a private directory under `/workspace`. Never fall back to host
storage for an unavailable VM or external session. Receipts retain digest/path,
not image bytes. Insert the quoted path into the original session's draft without
submitting it or overwriting intervening edits; handle UTF-16 cursor offsets.
Removing a draft path does not delete the upload; retention cleanup is not implemented.

## Build and checks

Use `shell.nix` and UV. Rust/Wormhole requires rustup nightly and the
wasm32-unknown-unknown target. The shell supplies Trunk, the WASM linker and a
NixOS loader wrapper. Follow the repository workflow's toolchain pins when
reproducing CI.

```sh
nix-shell shell.nix --arg hostOnly true
export CARGO_TARGET_DIR=target/rust-pwa
cargo build --locked
uv run tools/build-web.py
```

Candidate outputs are `target/rust-pwa/debug/demodex` and `web/.rust-dist`.
Use `-j 2` for memory-constrained builds. Run relevant checks inside the shell:

```sh
cargo test --locked -p demodex -p demodex-protocol --lib --bins
cargo test --locked -p demodex-web --bin demodex-web
RUSTC_WORKSPACE_WRAPPER="$PWD/tools/clippy-nightly.sh" cargo check --locked --all-targets
uv run tests/rust_web.py
uv run tests/targets.py
uv run tests/pwa.py
uv run tests/host.py
uv run tests/tailscale_auth.py
```

Browser suites require the configured Chrome executable. Host integration uses
real local Codex processes with disposable profiles/history and no model turns;
it covers execution, profile preservation, sandbox/cwd settings, images and
restart/resume. `DEMODEX_BIN` overrides its candidate binary.

Python fixtures use `tests/wormhole_client.py` and the native CLI. Its path-shaped
helper is test shorthand, not a compatibility API; real daemon traffic is Wormhole.
The core suite also covers native authentication, reconnects, notices, typed
results, large event pages, reader shutdown, static routes, legacy receipts across
database restart, and runtime shutdown draining.

The older `tests/vm_boot.py` runner still uses `target/debug/demodex`; the other
VM/smoke runners use the `target/rust-pwa/debug` candidate. Inspect paths before
running them; do not overwrite a live binary to satisfy a test. VM tests require
KVM, QEMU, SSH, Nix, Codex and
bubblewrap as appropriate; omit `hostOnly` for the shell's VM tools.

`uv run tests/vm_boot.py IMAGE.qcow2` checks disposable guest boot/reboot;
`uv run tests/managed.py IMAGE.qcow2` checks managed lifecycle and persistence
without model turns. The opt-in real-session runner makes paid model calls:
`uv run tests/real_sessions.py --run-real-sessions --app-server unix:///absolute/data/runtime/ipc/app.sock IMAGE.qcow2`.
Supply an already authenticated app-server; never copy credentials for testing.
The runner rebuilds a disposable guest, retains private `.demodex/live-*` data
and leaves the supplied app-server running.

## VM lifecycle

Build the pinned base with `nix build path:./nix#vm-image`. Create and run a VM:

```sh
cargo run --target-dir target/rust-pwa -- vm create --base /absolute/path/to/image.qcow2 --directory .demodex/work
cargo run --target-dir target/rust-pwa -- vm run .demodex/work
```

The parent directory must exist. Keep the immutable base at its backing path
and retain its Nix GC root while overlays depend on it. Each VM has a private
QCOW2 overlay, writable Nix store, guest root and separate SSH identity; only
the public key is injected into the guest through QEMU firmware configuration.
`/workspace` is guest-local. Default networking is isolated except for forwarded
loopback SSH; explicit `--network nat` allows outbound access, including host/LAN.

The manager imports the Codex runtime closure into the guest's private store
and starts its executor over SSH. Stop preserves disks; restart needs explicit
session reconnect. Manager shutdown stops owned processes. Without `--vm-image`,
first provisioning builds and roots the pinned base; supplied bases must be
rooted by their owner. Run from the checkout: default base building uses the
compile-time checkout path.

Host filesystem shares, host cache service, arbitrary remote-host provisioning
and a VM reset workflow are not implemented. If added, only the host manager
may grant shares; enforce read-only access in virtiofsd with explicit UID/GID
mapping. Host cache access must be read-only. VM reset and workspace deletion
must remain distinct and must never silently discard data.

## Serving and deployment

`--api-only` separates the daemon from static serving. The static process opens
no session database and starts no Codex processes:

```sh
target/rust-pwa/debug/demodex web --bind 127.0.0.1:4782 --directory web/.rust-dist
```

The usual layout is loopback Wormhole 4780, static UI 4782, and Tailscale Serve HTTPS
8443 with `/` and `/wormhole` routes. Retired `/api` routes return 404.
Identity-enabled WebSockets must go
through the private socket. Inspect existing Serve mappings before changes;
another application may already use port 443. Check installed Tailscale support
for Unix-socket proxies. The NixOS module exposes `apiOnly`, `allowedOrigins`,
`tailscaleUsers`, and `web` options; Serve configuration is separate.

Publish complete static releases atomically and retain assets needed by open
tabs. Keep daemon/frontend release selection separate. Changed protocol or schema
requires compatible releases at both ends. Inspect current session activity,
back up state and plan an idle restart before daemon deployment. Keep runtime
Nix dependencies rooted.

Historical `_Tasks/*/activate.py` and check scripts target old REST releases;
do not use them for protocol v16 or later rollouts. Prepare activity checks with
the native client and review matched daemon/PWA release paths before deployment.
The v16 refactor was verified locally without deployment, VM boot/provisioning,
or paid model inference.

For a standalone Pages frontend:

```sh
uv run tools/build-web.py --standalone --public-url /demodex/ --dist web/.pages-dist
uv run tests/pages.py
```

`.github/workflows/pages.yml` builds/tests the static PWA on pushes to main or
manual dispatch and deploys from main. Pages must use GitHub Actions as its
source. Publish only the static output; never runtime state or credentials.
Connections/tokens remain scoped to each browser origin and do not transfer
between per-instance UIs and Pages.

Deferred option: Tailscale Services could give each instance a dedicated name
such as `demodex-laptop.<tailnet>.ts.net` on HTTPS 443. This requires tagged
Service hosts and administrator configuration/approval. Preserve normal laptop
user identity; consider a separate tagged serving instance. Cross-tailnet node
sharing must not be assumed to share Services; a separate serving identity in
the client's tailnet is one possible approach. Recheck upstream support and
access policy before implementation. This option is not implemented.
See the [Services documentation](https://tailscale.com/docs/features/tailscale-services).
