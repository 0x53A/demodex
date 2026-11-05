# Demodex

Rust manager and mobile-first Rust/Yew PWA for Codex sessions and isolated NixOS
work VMs. Instances are independent; browsers connect directly to a selected
daemon. See `DESIGN.md` for UI conventions and `README.md` for user setup.

Keep this file concise and about the current state: architecture, invariants and
working instructions. Update the relevant section when behavior changes; do not
append release notes, protocol histories, completed-work reports or deployment
inventories. Keep task history and machine-specific records in `~/tasks/`.

## Code map and transport

- `src/`: core library, daemon, orchestration, persistence and target management.
- `src/service.rs`: transport-independent operations and durable receipts.
- `src/wormhole.rs`: authenticated actors; `src/http.rs`: static assets/upgrades.
- `crates/protocol/`: shared API and compatibility contract.
- `crates/client/`: native Wormhole client used by the CLI and Frosticus.
- `crates/web/`: Yew HTML/WASM frontend; small JS files handle PWA/voice glue.
- `src/claude/`: Claude account, runtime and inference adapter.
- `src/voice*.rs`, `crates/stt/`: voice transport, archives and private STT worker.
- `nix/host-service.nix`, `nix/stt-service.nix`: NixOS service modules.
- `tools/build-web.py`, `tests/`: frontend build and UV integration/browser checks.

Use Codex app-server JSON-RPC, never terminal scraping. The compatibility baseline
is codex-cli 0.166.0; its API is experimental. Preserve unknown events and report
unsupported requests explicitly. Packaging must not require a bundled zsh helper.

UI traffic uses typed ractor_wormhole calls and pushed notices at `/wormhole`, not
REST or Axum. A JSON hello checks VERSION and schema fingerprint before handshake
or authentication. Both peers reject mismatches. Keep wire declarations and record
serde encoding in `crates/protocol/src/wire.rs`; build.rs hashes normalized
declarations and locked transport dependencies. Do not introduce target-specific
wire shapes or move types into unhashed modules. Bump VERSION in
`crates/protocol/src/lib.rs` for semantic changes, including JSON/serde encoding
changes that the structural hash cannot detect. Deploy matching daemon, frontend
and native clients. Durable receipt encoding is versioned separately.

Storage and dispatch reuse shared session/event records without duplicate JSON
round-trips. Extensible upstream records retain serde_json::Value. Forms construct
shared commands directly. Render through Yew's Message/update/view model.
Frosticus at `../frost/codicillus-frosticus` consumes client/protocol crates by local
path with a separate ownership-enforcing member API; rebuild it when affected.

## Lifecycle and persistence invariants

Conversation identity, runtime, execution target and browser lifetime are separate.
The daemon owns RPC connections, executors and approvals across browser disconnects.

- Persist UUID mutation receipts before execution. Duplicates return recorded
  results. Interrupted/uncertain commands, uploads, prompts and approvals never
  replay automatically. Preserve existing SQLite data and legacy receipt decoding.
- Replacing an executor creates a fresh identity and invalidates its handles.
  Process handles remain generation-scoped; only explicitly verified durable
  file-host bindings can resolve files through a replacement executor.
- Lost-generation requests remain visibly unavailable. Questions and approvals
  stay pending until answered; timeouts/reconnects never submit defaults.
  Unsupported approval types require explicit JSON response editing.
- Never send a hidden model turn to apply settings, reconnect or recover state.
  Empty saved threads report missing rollout/history rather than being recreated.
- Runtime restart requires idle sessions, no decisions, queues, active goals,
  background terminals or unknown loaded threads. Reconnect explicitly afterward.

`Runtime::start(Config).await` owns storage, executors and monitoring;
`runtime.service()` returns cloneable handles. `Service::call(request_id, Operation)`
is trusted in-process access: its host application supplies authorization.
Await `runtime.shutdown()` before leaving Tokio. Shutdown rejects new calls, drains
admitted work and stops owned processes. Drop cleanup needs a live Tokio runtime.
All Service handles retain the state-directory lock, even after shutdown; release
them before reopening. Accepted commands finish and record receipts after a client
disconnects; connection actors and socket readers are cancelled together.

On daemon startup, read saved goals through Codex's `thread/goal/get` without
loading threads. Reconnect non-archived sessions whose saved goal is active,
regardless of their previous connection/turn status; no shutdown marker is needed.
Use the ordinary provider/target connect path, reject pending target changes, and
leave failed attempts visibly disconnected. Attempt once per startup, never retry
periodically or submit a prompt, goal mutation, queued message or approval. Shared
native goal storage also covers Claude; only an eligible Claude session starts its
runtime. `tests/restart_goals.py` and `tests/restart_goals_native.py` cover recovery.

The native client validates Hello before credentials, authenticates and subscribes.
`call` takes a receipt ID; `read` rejects mutations. There is no automatic retry or
reconnect. Closure promptly reports uncertainty and dropping releases readers.
Diagnostics use `demodex call --url ws://127.0.0.1:4780/wormhole --token-file PATH`
with JSON lines, e.g. `{"operation":"Sessions"}`. Supply stable `request_id` UUIDs
for mutations and inspect uncertainty with `{"operation":{"Receipt":{"id":"UUID"}}}`.
Never put tokens in URLs or arguments; logs go to stderr.

## Session settings and providers

Goal start, save, pause, resume, complete and clear are available during active
turns through Codex's goal API, without interrupting or sending an extra turn.
Start/resume still reject pending execution targets; disconnected, archived and
unsupported-goal states remain guarded. Tests in controls.rs and rust_web.py cover
active-turn mutations and the retained activation guard.

Sandbox, model, effort, tier, context-window and prompt settings persist separately
from the user's Codex configuration. Default model settings inherit runtime values;
missing catalogue metadata disables context overrides. Validate exact context
limits server-side without clamping; revalidate on model changes and resolve
Default again on resume. Sandbox changes are idle-only, using `sandbox` on
start/resume and `sandboxPolicy` on settings updates; approval stays on-request.
Invalid saved choices must produce validation errors, never broader access.

Per-session subagent limits are optional positive counts excluding the primary agent.
New Session passes `agents.max_concurrent_threads_per_session` through thread config;
Session controls use the guarded idle unsubscribe/resume path with saved history and
no active children. Blank inherits Codex configuration; no shared profile is edited.
`uv run tests/subagent_limit.py` checks real runtime instructions against local mock
inference; set DEMODEX_WEB to a matching build for mobile controls.

Prompt layers comprise per-model replacements, a shared appendix and Demodex
integration instructions, with revision-checked receipts. Preserve legacy complete
overrides until explicitly replaced. Applying prompts/context changes holds
lifecycle/settings locks, checks idle state, decisions, queue, goals and background
jobs, then unsubscribes and resumes without a turn. Reject empty history before
disconnection. Effort/tier-only and no-op model changes do not reconnect. An inactive
`systemError` may still permit controls. Model notifications must match thread and
generation. Prompt previews are bounded, read-only and use the selected executor;
global AGENTS inclusion has no independent Codex switch. Existing threads retain
saved tool schemas and prompt layers until explicitly updated.

PluginSettings discovers installed plugins from the selected runtime via `plugin/installed`;
never hardcode plugin names or edit the shared Codex profile. Server plugin exclusions
are copied atomically when creating a new session. Old and imported sessions retain
upstream settings. Explicit session changes require idle/decision/queue/goal/job guards,
use `thread/settings/update.disabledPluginIds`, and require same-generation confirmation.
Codex activates exclusions at the next turn; no hidden turn or reconnect is needed.
Saved selections are reapplied only when resume reports a different selection. Defaults
and session edits reject stale revisions/selections; missing plugin IDs remain visible.
The 0.166 response-schema comment claiming exclusions do not filter capabilities is stale;
real-runtime tests verify removal from model-visible tools. Run `uv run tests/plugin_settings.py`;
set DEMODEX_WEB to a matching build to include the mobile browser checks.

Native GPT sessions use the original runtime/profile and OpenAI provider directly,
without Claude catalogue/proxy/transport changes. Claude uses a separate lazy
app-server at `claude-runtime/ipc/app.sock`, a Claude-only catalogue and provider
ID `demodex_mixed`. Its loopback bridge key reaches the app-server only through
`DEMODEX_INFERENCE_KEY` (`env_http_headers`; the bwrap process environment is cleared and set explicitly),
never a command line. Both runtimes share targets and durable thread storage. Persist
provider bindings and inspect saved identity on import; cross-provider model
changes require a new session. Same-provider subagents work; cross-provider
delegation is unavailable. Claude failures must not stop native GPT operation.
`RestartClaudeRuntime` checks Claude sessions only; `RestartRuntime` checks both
because it replaces their shared host executor. Shutdown owns both children.

ClaudeUsage is an independent read-only operation for cached OAuth account quotas
from `/api/oauth/usage`. Normalize only five_hour/seven_day, preserving fractional
utilization and absent windows; never infer missing values as zero. Cache successes
and sanitized failures for five minutes by account generation, discard in-flight
results on account changes, and bound requests/body size. Reads must not start the
Claude runtime or block native usage/transcript reads. Header Codex/Claude graphs
show connected accounts and only reported 5h/weekly limits. Unit/mock coverage is
in src/claude/usage.rs; its ignored claude_usage_browser test runs tests/claude_usage.py
with DEMODEX_WEB pointing to a matching PWA build. No real credentials/inference.

Claude OAuth uses PKCE and a single-use expiring code#state transaction. Tokens
remain private in `claude/account.json` (0600); receipts retain only code digests.
Disconnect is local, not upstream revocation. The adapter uses HTTP/SSE, local
compaction, translated tools and signed-thinking replay; unsupported content and
truncation fail explicitly without retries. Hosted tools, deferred tool_search and
external image URLs are unsupported. Never route GPT requests through this adapter.
Older mixed GPT threads require explicit migration, not guessed provider routing,
recreation or direct Codex SQLite/rollout edits. Migration procedure and script:
`~/tasks/0196-demodex-native-codex-isolation/`.

Local CLI takeover is explicit and receipted. Recheck the reviewed daemon identity
and thread list; verify socket peer PID/UID and boot/start identity, pin a pidfd,
signal SIGTERM through it and wait for that exact process before resuming. Fail
closed without pidfd support. Never kill by numeric PID/name, use SIGKILL, delete
locks or stop an unreviewed replacement/shared daemon automatically.

## Execution targets and isolation

Targets are daemon-scoped and may be shared. Stable registry IDs and per-session
working directories are distinct from executor generations. Host access requires
`--host-workspace`; never fall back from an unavailable remote target to host.
Blank creation/import directories preserve defaults; explicit directories must
exist and be absolute. Reimport cannot retarget an attachment. Display the active
executor directory, not the resumed thread's historical cwd.

Target changes require connection, no decisions/queue and an inactive/paused goal.
SelectTargets is idle-only; ChangeTargets stages next-turn changes or interrupts
and waits for confirmed completion. Register/validate before persisting. Current
and staged selections both retain registry usage. Steering keeps current targets
and directories. Apply environments only on the next explicit `turn/start`, not
with a cwd settings patch; block goal/queue resumption while application is pending.
Reconnects and detail reads clear non-staged pending state only when the same
thread reports the exact ordered executor IDs and directories (excluding Codex's
built-in local environment). Missing/mismatched snapshots and explicitly staged
changes remain pending; autonomous goal turns alone do not confirm application.
The first target is primary; empty
selection explicitly disables execution targets.

- VMs use private QCOW2 overlays, stores, guest roots and SSH identities; only the
  public key enters firmware configuration. `/workspace` is guest-local. Keep base
  images and their Nix GC roots while overlays depend on them. Default networking
  exposes only loopback SSH; explicit NAT permits outbound host/LAN access. Stop
  preserves disks; restart requires reconnect. Delete requires stopped state and
  no current/staged selections, preserving shared bases and conversations. Detach
  checks idle/decision/queue/goal/job guards; disconnected attachments require stop.
  Host shares, host cache service and reset are not implemented.
- OCI containers use locally available Docker/Podman images, private persistent
  workspace/home, isolated per-container bridges, resource limits, dropped
  capabilities and loopback executor ports. Never mount host home or engine socket.
  Nix Codex may bind the host store read-only. Rootless Podman uses keep-id;
  Podman 6+/Netavark strict isolation is required. Inspection failures preserve
  ownership and report unknown state. Require explicit danger-full-access.
- SSH uses OpenSSH foreground commands and existing SFTP v3, strict host-key checks
  and batch authentication. No helper is installed remotely. Login shells do not
  parse command/environment values; scripts go via stdin to a fixed launcher.
  Require Linux, sh, env (including env -0), cat and danger-full-access. Command
  stdin is EOF; PTYs, stdin writes, remote signals and resumable process handles
  are unsupported. There is no adapter command timeout. Cancellation closes SSH;
  remote outcomes may remain uncertain and must never be replayed. Named tmux jobs
  may be managed explicitly if installed; Demodex does not track or clean them up.
  SFTP whole-file operations are bounded at 32 MiB; streaming, recursive operations
  and strict no-follow guarantees are unsupported. Replacement disconnects paused
  attached sessions and creates a new identity. VM exec-server is a separate backend.

Loopback is the default bind; login uses an owner-only random token. Only the
private 0600 `tailscale.sock` trusts an exact Tailscale-User-Login plus allowed
Origin. TCP ignores identity headers; explicitly invalid tokens always fail.
Cross-origin access requires exact origins, never wildcards. An allowed Pages
origin trusts all apps on that origin. Keep the Unix proxy alias off the TCP router.

Isolated VM mode uses a bubblewrap runtime whose filesystem excludes host home,
projects, SSH credentials and engine sockets; the Nix store is read-only and
networking shared. Native host mode carries the service user's full authority.
`--codex-home` explicitly shares a profile; never copy credentials or assume
processes coordinate refresh. Dedicated login uses chatgptDeviceCode. External
app-server/executor isolation belongs to the operator.

## Browser, history and content

Follow `DESIGN.md`. Keep drafts/tab navigation host-specific and credentials out of
history. Saved connections/tokens are origin-local; Forget disconnects that client,
not other tabs. Preserve drafts through updates, independent transcript scrolling
and conditional following. Service workers cache verified static assets only,
never credentials, API responses or conversations; no forced reloads or replay.
Vertical scrollports suppress horizontal touch panning/overscroll while preserving
pinch zoom and nested horizontal content. `uv run tests/scroll_views.py` checks the
production CSS with touch gestures; actual Safari rubber-banding needs device testing.

Coalesce notices per resource and reject stale navigation/generation responses.
Content notices refresh the selected transcript; state notices refresh summaries
and detail; runtime notices do not reload transcripts/targets. Session summaries
own metadata. Conversation pages use stable high-water/item cursors. Pages target
512 KiB and 200 items; indivisible oversized items are never truncated. A versioned
per-item index in the existing SQLite database serves pages directly. A runtime-owned
worker backfills/catches up in resumable batches; changed rows and progress commit
together. Retain item versions for published cursors. Raw events stay authoritative;
pre-index cursors fall back to bounded cached replay. Bump INDEX_VERSION in
src/store/conversation.rs when projection semantics/dependencies change. Keep the
shared transcript dependency selection and unfinished/steering state complete.
Reload the projection
if a resume snapshot arrives while older pages remain unloaded; invalidate pending
older reads. Full-text search is bounded, literal, case-insensitive saved-history
search without Codex resume/inference. Stop/query changes invalidate further reads.

Never insert model HTML or arbitrary URLs into active attributes. Preserve exact
source/raw copy. MathML is allowlisted; Mermaid uses a pinned vendored bundle in a
network-disabled opaque-origin sandbox. SVG previews validate a bounded XML subset
in `crates/web/src/svg.rs` and render in img documents, never inline DOM/object/
iframe. Preserve bounds and rejection of scripts, external resources, recursive
paint references and unsafe animation. Unsupported SVG rejects the whole preview.
Web links show parsed destinations explicitly; external images require separate
Load confirmation. Tagged message buttons only insert text at the retained draft
cursor; sending remains manual.

Completed image-view steps offer a compact View image button using the same file
preview and metadata capture as assistant links. Older steps without captured metadata
remain explicitly unavailable; never infer their executor from current attachments.
Completed assistant messages capture bounded file metadata once through recorded
executors for both ordinary file links and local Markdown images. Non-uploaded
local images open the same explicit file preview; uploaded images retain verified
thumbnails. Older image links without metadata remain unavailable.
Never replay checks automatically. Content reads require Show/Download,
regular files and the 4 MiB bound. Durable file-host bindings verify registry target
plus machine-id/UID fingerprint on the same access connection. Pairing is explicit
and cannot replace an existing fingerprint; only orchestration registries confer
binding authority. Legacy unbound snapshots require the original generation.
Refresh explicitly rechecks one destination without reading content. Parse positive
:line[:column] before percent decoding; literal filename colons use %3A.

Uploads use authenticated receipts: UploadImage validates PNG/JPEG/GIF/WebP
signatures and 32 MiB; UploadFile preserves original bytes/safe names up to 32 MiB.
Receipts retain digest/path, not bytes. Image reads verify upload digest and original
executor selection. Insert into the original draft preserving intervening edits
and UTF-16 selection offsets; never auto-send. Share intake stages locally in
bounded IndexedDB for up to 24 hours, separately from static caches; cancellation
retains the incoming share. Upload retention cleanup is not implemented.

Notifications are explicit, receipted chat events with an at-most-once encrypted
Web Push outbox. Provider acceptance is not delivery; no devices means disabled.
Emergency calls are urgent notifications, not phone calls. Bug reports stay in the
per-daemon inbox; its new-report watermark advances only after loading the newest
page. Usage/reset credits are read-only upstream reports; missing values remain
unknown. Context cache warmth is an age heuristic, not confirmed cache/billing data.
Project Git snapshots are bounded, read-only and disable hooks, optional writes,
submodule recursion and lazy fetches; remote checks never fall back to host.

## Voice

Private STT uses the Rust Unix-socket worker and SSH bridge; see
`crates/stt/README.md` and its shell.nix. Model GGUF is provisioned separately;
no runtime cloud audio/model requests. `--stt-config`/`services.demodex.sttConfig`
enables it with strict SSH verification; no configuration means disabled.

Voice is an authenticated, connection-owned Api::Voice stream, outside operation
receipts/transcript events. Bound and sequence 16 kHz mono PCM chunks. Browser
capture uses getUserMedia/AudioWorklet. Stop inserts XML-escaped `<tts>` text into
the draft without wording corrections or submission. Cancel/navigation/hiding/
disconnect stops capture; keep the worker slot until cancellation completes.
While capturing, request a browser screen wake lock; release it on every teardown,
including late grants after cancellation. Denial/unsupported APIs/system release
show a nonfatal warning. Hiding still cancels capture; wake lock is not permission
to record in the background. `tests/voice_wake_lock.py` covers browser lifecycle.

Every accepted stream is retained indefinitely under private
`<data-dir>/voice-recordings/<uuid>/` as audio.flac and metadata.json, including
received prefixes of failed/cancelled streams. Audio is not stored in SQLite or
sent to the agent; no pruning/deletion UI exists. Durably spool before forwarding;
acknowledge only saved samples. Atomically publish FLAC/metadata, delete PCM only
after successful encoding, recover interrupted spools and drain jobs on shutdown.

## Build, checks and deployment

Use `shell.nix`, rustup nightly, wasm32-unknown-unknown and UV; no Node/pnpm is
needed. Follow CI toolchain pins. The shell supplies Trunk/linker/NixOS loader.

```sh
nix-shell shell.nix --arg hostOnly true
export CARGO_TARGET_DIR=target/rust-pwa
cargo build --locked
uv run tools/build-web.py
cargo test --locked -p demodex -p demodex-protocol --lib --bins
cargo test --locked -p demodex-web --bin demodex-web
RUSTC_WORKSPACE_WRAPPER="$PWD/tools/clippy-nightly.sh" cargo check --locked --all-targets
```

Use `-j 2` for memory constraints. Run relevant `uv run tests/<suite>.py` checks:
`rust_web`, `pwa`, `rich_messages`, `share`, `host`, `targets`, `tailscale_auth`,
`prompt_settings`, `context_window`, `runtime_features`, `runtime_startup`,
`provider_isolation`, `file_hosts`, `project_git`, `takeover`, `notifications`,
`vm_delete`, `ssh`, `container`, `voice`. Inspect each fixture's build-path/env
requirements; browser tests need Chrome. Most runtime suites use disposable real
Codex profiles without inference. Voice also has demodex-stt and voice library
unit tests. Never copy credentials for testing or overwrite a live binary.

VM tests need KVM/QEMU/SSH/Nix/Codex/bubblewrap as applicable; omit hostOnly for
VM tools. Build the pinned base with `nix build path:./nix#vm-image`. Check runner
binary paths before using `vm_boot.py` or `managed.py`. `real_sessions.py` is opt-in
and makes paid model calls. History benchmarks use `tests/history_benchmark.py`
with an optimized binary and disposable daemon; keep private snapshots out of Git.

Development outputs are `target/rust-pwa/debug/demodex` and `web/.rust-dist`.
Deployment uses `cargo build --release --locked` and the release binary; debug
history reconstruction is much slower. `nix/package.nix` builds the native daemon
with a caller-supplied nightly rustPlatform; automatic VM provisioning needs a
runtime checkout containing nix/ and a rooted base image.

`--api-only` separates the daemon from static serving (`demodex web --bind
127.0.0.1:4782 --directory web/.rust-dist`); the static process opens no session DB
or Codex processes. Usual routing is loopback Wormhole 4780/static 4782 through
Tailscale Serve HTTPS 8443. Inspect existing mappings before changes; identity
WebSockets must use the private socket. Never use old REST deployment scripts.
Publish complete static releases atomically, retaining assets for open tabs.
Check activity, back up state and plan idle daemon restarts; root Nix dependencies.

Standalone Pages builds use `uv run tools/build-web.py --standalone --public-url
/demodex/ --dist web/.pages-dist` and `uv run tests/pages.py`. The Pages workflow
publishes static output only, never state/credentials; connections remain scoped
to each frontend origin.
