# Demodex interface

Small, deliberate, slightly odd. A session console, not a decorative dashboard.

- Dark charcoal background, warm pale text, signal-orange highlights.
- Rectangular controls with fine borders; no pill cards or gradient backgrounds.
- Monospace labels/status, readable sans-serif conversation text.
- Text accompanies every status indicator. Waiting for a human is orange;
  active work green; disconnected muted; failures red.
- Mobile: session list and conversation are separate screens with an explicit
  back action. Desktop: persistent sidebar and main conversation.
- Approval/question cards are visually distinct and remain until resolved.
  No default answer, time limit, automatic selection, or implicit dismissal.
- Tool output is collapsed by default; preserve plain text verbatim. Never
  render remote output or model text as raw HTML.
- Composer and primary controls remain reachable on narrow displays.
- The viewport contains the application frame: header, session heading, desktop
  sidebar, and composer stay in place. The transcript scrolls between them;
  long session lists and other screens scroll within their own panels.
- Follow new transcript output only while the reader is at the bottom; preserve
  their position as soon as they scroll up, even a little. A persistent Jump to
  latest action resumes following explicitly. Keep the scroll region keyboard accessible.
- Show a text-labelled working indicator after the latest message, or a waiting
  indicator when input is required. Respect reduced-motion preferences.
- Offer Connect / resume only for disconnected sessions. Keep session settings
  collapsed by default to leave room for the conversation.
- No terminal is required to use the main flow. Show protocol details only in
  expandable diagnostic views or connection configuration.
- PWA manifest and local assets; no third-party fonts, scripts, or telemetry.
- PWA releases download automatically. A bounded launch-time check may apply an
  update before interaction begins; an active page shows a persistent update
  notice and reloads only on request. Other tabs keep running.
- Persist unsent drafts, question answers, and selected session in tab-scoped
  sessionStorage across reloads. Never submit or replay them automatically.
- Offline cache contains only the verified static app shell. Session/API data
  and credentials never enter Cache Storage. Lost connections visibly mark
  displayed state as stale and retry reads without replaying writes.
- HTTPS terminates at Tailscale, independently on each host. There is no central
  service that coordinates the separate installations.

## Session overview and context

- Each server shows one sparse folder tree, grouped by project path, never by
  executor generation. Each session appears once: at its reported project, or
  its primary executor directory. Executor names appear on session entries.
  Only paths with sessions and their ancestors appear; no filesystem scan is
  needed. Compress ancestry with no sessions to keep deep paths readable on mobile.
- Each attachment has a persisted generated adjective/subject name and icon.
  Preserve its existing session title separately. Names are friendly labels, not
  unique identifiers; always retain UUIDs as the authoritative identity.
- Keep the Codex thread UUID visible in the conversation heading. Show labelled,
  selectable Codex thread and Demodex session UUID fields in the context panel.
- `demodex.set_user_visible_session_context(environment_id, path, description="")`
  reports display metadata for the calling session. Use an absolute project root
  in an attached environment, with an optional one-line description. It never
  changes execution directories, sandbox settings, identity, or another session.
- `demodex.get_session_context()` supplies identity, current display metadata,
  and attached environment IDs/directories without transport endpoints or secrets.
- Agent-reported locations take precedence for tree placement. Without a report
  for a currently attached environment, label the location as the executor
  directory. Do not infer project identity from occasional shell commands.
- Codex 0.154.0 registers these dynamic tools on new threads and restores them
  from rollout metadata on resume. Imported/older threads have no registration
  path in the current resume schema; clearly show the fallback in their UI.
- Add the Demodex explanation through developer instructions while preserving
  effective configured developer instructions. Do not write the user's profile.
- Context mutations and their UUID receipts commit atomically. Duplicate tool
  call IDs return the recorded response and cannot overwrite a newer report.

## Session commands

The Session controls button and `/model`, `/goal`, `/status`, `/help` open the
same UI. Recognized commands do not become prompts or queued messages. `/goal
OBJECTIVE` only fills the form; saving and starting require explicit actions.
Unknown command names are rejected locally, with the draft retained.

The model picker uses the attached app-server's paginated `model/list` catalog,
including its reasoning efforts and service tiers. Selection is separate from
the accepted settings. Idle changes use `thread/settings/update`; only a matching
generation's notification confirms the result. Persist accepted choices for
resume without editing the Codex profile. Unconfirmed changes remain visible.

Goals are Codex's own thread goals, read with `thread/goal/get` and changed with
`thread/goal/set` or `thread/goal/clear`. Show state, token usage, elapsed time and
budget. Save stores a paused goal; starting/resuming is explicit. Pause remains
available during a turn and prevents subsequent goal turns, without pretending
to interrupt the current turn. Blank budget leaves the current budget unchanged.
Do not invent a budget. Display unsupported API errors without breaking chat.

TUI parity inventory (Codex 0.154.0):

| TUI feature | App-server equivalent | Demodex |
| --- | --- | --- |
| Model / reasoning / speed tier | `model/list`, `thread/settings/update` | Session controls |
| Goal | `thread/goal/get`, `set`, `clear` | Session controls |
| Permissions | `thread/settings/update` | Existing sandbox settings; approval policy remains on-request |
| Resume / new | `thread/list`, `thread/resume`, `thread/start` | Existing environment/session forms |
| Queue | `thread/queue/*` | Existing composer queue |
| Status | Thread settings, goal, runtime and account reads | Context, controls and environment screens |
| Compact | `thread/compact/start` | Not yet exposed |
| Fork | `thread/fork` | Not yet exposed |
| Review | `review/start` | Not yet exposed |
| Plan mode | `collaborationMode/list`, `thread/settings/update` | Not yet exposed |
| Rename | `thread/name/set` | Not yet exposed; separate from generated identity |
| Skills / apps | `skills/list`, `app/list` and related APIs | Not yet exposed |
| Background terminals | `thread/backgroundTerminals/*` | Persistent count, separate modal, stop individual/all listed |

This is a mapping of capabilities, not a promise that raw TUI command text is
accepted by app-server. Terminal-local commands (theme, editor, keybindings,
screen clearing) need browser-specific designs.

- Enter inserts a newline in the message composer; Shift+Enter sends. Composition
  events and repeated keydown events never send. The shortcut and Send button share
  eligibility checks and preserve drafts when delivery fails.
- Send and Shift+Enter remain available during agent work and pending questions.
  Send starts a turn when idle and uses `turn/steer` with `expectedTurnId` during
  active work. Steering adds input to that turn at Codex's next processing boundary.
  If Codex explicitly rejects steering because no active turn remains, verify
  idle and send the same message as a normal turn under the original mutation
  receipt. Never fall back on a timeout, connection loss, or another rejection;
  preserve the draft when delivery remains uncertain or unsupported.
- Queue for later is a separate button available during active work. It uses
  `thread/queue/add` for a subsequent turn after the current turn finishes.
  Neither sending nor queueing answers a question or approval. Interrupt pauses
  queue advancement; resuming the queue is explicit. Browser reconnect never
  resubmits input, and UUID receipts deduplicate both Send and Queue for later.

Session archiving is local to each Demodex server and preserves conversation
history, identity, drafts and execution configuration. Only stopped sessions
can be archived: live sessions must be idle, have no pending decisions or queued
messages, and have no active goal. Archive never interrupts a turn or pauses a
goal. The operator uses Interrupt and the existing queue/goal controls first.
Archived sessions appear in a collapsed section of the overview and provide a
Restore action. Restore does not resume work. Archived sessions cannot start
new work through Demodex until restored; externally started turns make their
session visible again. Archive state persists across daemon restarts.

## Execution target selection

Environments contains the daemon's shared target registry: configured host,
managed VMs and named external executor endpoints, with attached sessions shown.
A session's Execution targets picker selects an ordered set and working directory
for each target. Multiple conversations can use the same VM. The first target
receives image uploads; selecting no targets explicitly disables executor tools.

Selection is editable only while connected and idle, with no pending decisions,
queued work or active goal. Saving does not start a turn. Display “Targets saved”
until the next explicit message applies the list; goal and queue resumption must
not bypass that state. VM loss preserves selections and requires reconnect.

## Independent interaction panels

Session controls open a native modal dialog with its own scroll area and fixed
Close action. Escape closes it, focus stays inside while open, and returns to
the opener on close. Model/goal controls, context and UUIDs, execution targets,
sandbox settings and archive/restore are outside the transcript. Opening or
closing controls never resets conversation scroll position or follow mode.
Protocol diagnostics open separately and are serialized only when requested.
Group session behavior (model and goal), execution (targets and sandbox),
context/identity, and history actions in that order. Keep archive/restore apart
from routine execution settings. Server Settings starts with account access,
then shared targets and VM lifecycle, with device installation last.

Pending questions/approvals and queued messages occupy a bounded independently
scrolling panel above the composer. They remain reachable regardless of history
length; closing settings never answers or dismisses a pending decision. The
header, navigation, transcript toolbar and composer stay within the viewport.
Long titles are visually truncated; full metadata stays in session controls.
Conversation rendering is isolated from draft/control edits using immutable
projected message chunks, so typing does not rebuild the full message tree.
Project incoming events once, update items by stable ID, and reuse unchanged
chunks and message components. Streamed deltas affect only their item; completion
replaces that item's content. Resume snapshots merge without dropping older
turns omitted after compaction. Preserve tool expansion, text selection and
scroll position for unchanged messages. Keep the raw event log for diagnostics.
This is incremental rendering, not virtualization: initial loading and retained
browser history still scale with the full conversation.

## Usage indicators

The server usage strip shows the weekly quota reported by its Codex runtime
account via `account/rateLimits/read`. It displays only windows explicitly
reported as 10,080 minutes; it never guesses from primary/secondary position
or substitutes a five-hour window. The Codex bucket comes first; additional
weekly buckets remain separate in the expanded details. Each includes percent
used and its reported reset time. Usage belongs to the account, so servers
using one account can display the same quota. Reads are cached for 60 seconds,
keyed by runtime connection and account. Errors and missing windows display
unavailable, never zero. The UI shows the check time and handles disconnection.

Each session shows context usage in its tree entry and fixed transcript toolbar.
Controls provide the exact last-reported token count and model context window.
Use `thread/tokenUsage/updated.tokenUsage.last.totalTokens`, not cumulative
`total.totalTokens`, divided by `modelContextWindow` without assuming a model
capacity or applying an undocumented baseline adjustment. Missing capacity or
usage is unavailable. Values can fall after compaction and persist across
restarts; older sessions recover their latest report from persisted events.
These are last-reported values, not a live count of an unsent draft.

Background terminals have a persistent transcript-toolbar count and a separate
scrolling modal (`/ps` and `/stop` open it). Stop all operates on the displayed
selection, not processes created afterward. Stops carry the app-server connection
generation and process/item IDs and use mutation receipts. Codex 0.154 reports
command/cwd but no execution-target identity for background terminals: show
“Target not reported” rather than infer it from mutable session targets. This
panel covers Codex background terminals, not arbitrary host processes or jobs
started outside Codex. Unavailable/unsupported lists must not display zero.

## Navigation and session creation

The fixed header owns Current Server, Connections and Target host. Connection
management opens a modal from the header. The sidebar contains Server Settings,
then Sessions, New Session, and the folder tree. There is no External Session UI.

New Session opens its own widget without navigating away from the conversation.
It selects registered host, SSH, VM or external executors, with explicit working
directories and a primary target. SSH-only creation requires danger-full-access.
Inline VM provisioning adds the resulting VM to the draft selection; creating
the session remains a separate explicit action. Draft fields survive closing.
Saved host-thread discovery and explicit resume also live in this widget.
Creation and resume have independent name and sandbox drafts; completing either
form leaves the other intact. Setup and session-control drafts survive switching
servers and remain scoped to their original server. Goal status actions preserve
unsaved objective and budget edits.
Server Settings retains account/login, target registration and VM lifecycle.

New sessions persist daemon runtime ownership separately from execution targets.
Creating or reconnecting an SSH-only session does not attach a host or VM target.
The existing diagnostic external app-server API remains available.
