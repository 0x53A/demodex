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
  Command/file approvals lead with the reason, exact command when supplied,
  and reported executor/directory or requested write root. Keep requested extra
  permissions visible and the complete protocol request in expandable details.
  Approve once, Decline and Cancel turn are separate touch-sized actions; show
  pending, sending, delivered and unavailable states in plain language.
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
- A Close session view action returns to the unselected overview without changing
  the session or its drafts. The unselected state persists across reloads, even
  when archived sessions exist; browser back/forward restores navigation.
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
- `demodex.set_session_identity(title?, name?, icon?)` updates only the calling
  session's Demodex title and display identity. Omitted fields stay unchanged.
  Validate the entire patch before committing it with its receipt; preserve UUIDs,
  Codex thread names, reported context, and execution settings.
- `demodex.get_session_context()` supplies title, identity, current display metadata,
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
budget. Start goal saves and activates in one call; Save paused is a secondary
action. Setting or replacing an objective is allowed during a turn. Pause remains
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

- By default, Enter inserts a newline and Shift+Enter sends. The “Enter sends”
  checkbox below the composer reverses these keys; its preference survives reload
  in the current tab and follows host switches. The action row wraps on narrow
  screens. Composition
  events and repeated keydown events never send. The shortcut and Send button share
  eligibility checks and preserve drafts when delivery fails.
- Send and the selected send shortcut remain available during agent work and pending questions.
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
The session tree exposes archive as a separate [x] button with an accessible
Archive label. Archived sessions appear in a collapsed section of the overview
and provide a Restore action beside each entry. Restore does not resume work. Archived sessions cannot start
new work through Demodex until restored; externally started turns make their
session visible again. Archive state persists across daemon restarts.

## Execution target selection

Environments contains the daemon's shared target registry: configured host,
managed VMs, managed containers and named external executor endpoints, with
attached sessions shown.
A session's Execution targets picker selects an ordered set and working directory
for each target. Multiple conversations can use the same VM. The first target
receives image uploads; selecting no targets explicitly disables executor tools.

Selection is editable while connected, with no pending decisions, queued work
or active goal. During a turn, show “Save for next turn” and “Interrupt and save”.
The former preserves the current turn; the latter waits for confirmed interruption
and idle state before saving. Acknowledgement alone is insufficient, and errors
or timeouts never trigger a replay or silently save the change. Idle sessions
retain “Save executors and directories”. Working-directory inputs and the directory
browser are grouped under a visible “Working directories” heading in Session Controls.
Directory-only edits follow the same next-turn boundary, without a session restart.
Adding a new private SSH executor still requires idle.

Saving does not start a turn or terminate background jobs. Keep pending selection
separate from effective targets, and show the current turn's targets when a change
is pending. A composer notice explains that Send still steers active work with
its current targets. Pause image uploads during active work with pending targets
to avoid uploading a path into an environment that turn cannot access.
The next explicit new turn applies the list; goal and queue resumption must not
bypass that state. VM or container loss preserves selections and requires reconnect.

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
then shared targets, container and VM lifecycle.

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

Conversation items use readable labels and show reported tool status, command
exit codes and duration, affected file paths with collapsed diffs, searches,
reasoning summaries, streamed plan text and step progress. Tool progress, retry
notices and failed/interrupted turns appear in the transcript. Session errors
remain visible outside controls; connection loss explicitly leaves activity
unconfirmed. Raw details remain expandable for unfamiliar item types. These
indicators report Codex events, never infer success from silence or elapsed time.

## Usage indicators

The header usage summary shows weekly remaining quota reported by its Codex runtime
account via `account/rateLimits/read`. The collapsed summary uses only windows explicitly
reported as 10,080 minutes; it never guesses from primary/secondary position
or substitutes a five-hour window. The Codex bucket comes first; additional
weekly buckets remain separate in the expanded details. The expanded view also includes all other reported windows. Each uses a square,
green meter of remaining capacity and a relative reset countdown; show Last updated
at the bottom. Usage belongs to the account, so servers
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

The fixed header owns Current Server, Connections, weekly usage and Server Settings
(visible while connected). Connection management opens a compact list modal:
each row connects when selected, with Edit and Delete actions on the right.
A dashed + connection row opens a second modal, as does Edit. Closing the editor
returns to the list; saving connects and persists the entry after success.
The sidebar contains Sessions, New Session, and the folder tree.
There is no External Session UI.

New Session opens its own widget without navigating away from the conversation.
It selects registered host, SSH, container, VM or external executors, with explicit
working directories and a primary target. SSH and container executors require
danger-full-access.
Inline VM provisioning adds the resulting VM to the draft selection; creating
the session remains a separate explicit action. Closing discards the session draft.
Saved host-thread discovery and explicit resume also live in this widget.
Creation and resume have independent name and sandbox drafts within one opening.
Closing, completing, or navigating away discards both. Session controls reopen
from saved settings. Goal status actions preserve unsaved objective and budget
edits while the dialog stays open.
Server Settings opens a content-sized modal over the current session or overview.
Closing it preserves the selected session, transcript position and active sign-in,
while discarding unsubmitted settings edits.
It does not reopen automatically on reload. It retains account/login, target
registration and VM/container lifecycle.
Use the DDU egui console's density: 10 px section padding, 6–8 px row gaps,
small monospace headings with an orange tick, and thin rectangular borders.
Account status fits one wrapping row. Feature flags expand into a striped list
within the settings dialog scroll area, leaving 32px above and below the dialog.
Descriptions stay visible; restart details and the explicit action expand separately.
Global executors have individual borders, with container and VM management side by side on
wide screens and stacked on mobile. Preserve larger controls for touch.

New sessions persist daemon runtime ownership separately from execution targets.
Creating or reconnecting an SSH-only session does not attach a host or VM target.
The existing diagnostic external app-server API remains available.

## Compact session workflow

Server Settings contains System prompts. Per-model replacements use a dashed add
button, model selector, pencil edit button and remove button. The shared appendix
follows either the current Codex/profile base or the model replacement. Demodex
integration instructions remain a separate editable layer. Editors offer Edit and
Diff modes using one draft; the diff compares the latest fetched default on the
left with an editable replacement on the right. Changed lines are red/green;
mobile stacks the two sides. Sources, profile overrides and changed-default notices
are explicit. Reset restores the current baseline; Cancel discards the editor draft.
Save persists the prompt settings without changing an active conversation.

New Session can inherit or override project-instruction inclusion. Session controls
offer inclusion checkboxes and an explicit Apply prompts and reconnect action.
New managed sessions initially use runtime defaults for model, reasoning effort
and service tier. Choosing a runtime-catalogue model enables explicit effort and
tier settings; the daemon validates the complete choice. Effort/tier-only changes
to an existing session do not reconnect it or apply pending prompt edits.
Global instruction inclusion is shown checked and unavailable because Codex does not
expose an independent switch. Instruction file previews are read-only and show exact
paths; these current-file previews are distinct from the source paths Codex reported
at connection. Prompt settings, editor drafts and previews follow normal dialog and
connection lifetimes. Tool selection is a separate feature.

The header reads // DEMODEX, Connections, connection status and server name,
usage, and Server settings. Keep descriptive account prose out of usage.
Session tree entries lead with the actual title, then generated agent identity,
status and executors, context usage, and a blue background-terminal count when
nonzero. Unknown counts remain explicit. Archive is a red × with an accessible
label. Session headings keep title, agent name, thread UUID and status in a
compact wrapping line. Controls and background-terminal actions live beside
composer buttons. Jump to latest is anchored at the transcript's bottom right
and exists only while scrolled away from the bottom.
New Session accepts an optional name. Resume is a checkbox switching the single
bottom submit action. Search opens a separate picker and selecting a thread
fills its ID, name and prior directory without submitting anything.
VM and container provisioning opens nested editors from the target list and
selects the result in the draft. SSH can be staged privately and attached after
creation; failures leave a session with an explicit retry form. Shared SSH is
registered separately in Server Settings.
Session controls use compact framed sections and aligned labels, matching
Server Settings while retaining touch-sized buttons.

## Compact activity and directory navigation

Adjacent tool/reasoning items form a collapsed activity group between messages.
Keep stable group/item IDs, original order, and expansion across streaming updates;
unreported outcomes and failures remain visible in the summary. Commands preview
five lines with full output expandable. Unified diffs use escaped text, coloured
additions/deletions and old/new line numbers. Preserve all raw output.

Working-directory fields for registered targets offer a nested directory picker.
It browses that executor only, permits typed paths, and requires explicit selection.
Selection edits the draft; submitting session/target changes remains separate.
Goals appear beside background activity in the overview and composer controls.

## Shared setup components

`ui::AddButton` opens creation/editing overlays; all add affordances share the
8px dash / 5px gap CSS tokens, including Connections and executor setup. Submit
buttons inside those overlays remain solid. `ui::SectionTitle` supplies the
orange section accent; `ui::FieldAction` aligns directory inputs and Browse.
Use these components instead of local dashed borders or ad-hoc form rows.
Creation overlays are content-sized and close on success. Closing any dialog
(including navigation) discards its unsubmitted edits; reopening uses defaults
or saved settings. Nested pickers preserve the still-open parent's draft, and
accepted staged SSH belongs to the New Session draft until submitted. Form drafts
are not restored after reload. Conversation drafts and pending decisions remain
separate. Resume starts with an empty thread ID; an empty resume form cannot submit.
Late responses cannot refill or close a different dialog opening. Submitted
operations and their receipts survive dismissal. Failed SSH attempts discard
form data; a successfully created session remains if SSH attachment fails.
Server settings retains an active device-login code across closing and reopening;
closing the dialog does not cancel sign-in or start another attempt.

- Stars are saved on the server and sort first within each project folder.
  Reorder mode exposes left drag handles (mouse, touch, or arrow keys). Drops
  reorder only siblings with the same star and archive state; stale orders fail.
  The pencil button opens that session’s controls; all corner actions have
  independent keyboard focus and hover feedback.
- Session cards reserve three activity lines for goal, background terminals and subagents, even
  when empty. Titles use the labelled status colour. Archive/restore occupies a
  separately focusable upper-right segment with a shared outline and thin divider.
- Steering messages remain in the transcript labelled “Waiting for the next tool
  call” until Codex reports the matching user message; never infer consumption
  merely from unrelated tool activity.

Goal setup defaults to explicit Start goal; Save paused is a secondary action.
Both can update objectives during active turns through Codex's goal API without
interrupting work or sending a hidden turn. Starting is blocked while execution
target changes await an explicit message. Show blocking conditions as yellow,
bold warnings at the normal text size only while they apply.

Execution targets and sandbox settings are always expanded inside Session controls.
Keep operation-specific blockers visible only while they apply; sandbox changes
remain idle-only and paused saved goals do not need to be cleared.

Context and quota meters show remaining capacity, anchored to the right. UUIDs
remain visible in Context and identity. Restart warnings list current blocker
counts; background terminals use compact command/cwd rows with IDs in Diagnostics.

Rich messages render Markdown tables, headings, lists, task lists, code and link labels,
plus LaTeX as native MathML. Formatted messages expose a checked Format Markdown
control and Copy raw below their content; disabling formatting shows the exact
original source. Preserve that choice as new messages arrive. Keep keyed message
chunks in a stable list container so unrelated updates do not remount them.

Never insert model HTML into the app document. Links are numbered inline buttons
with icon, index and title, an exact-destination hover tooltip, plus a destination list at the end of each message.
An operator-opened HTTP(S) popup exposes the full parsed URL with its hostname
bold before navigation. Reject malformed/control characters and embedded
credentials; explain normalization and Punycode. Unsupported schemes remain
inert. Images remain omitted; MathML and Mermaid supply no active destinations.
Raw view/copy preserves exact source. Rendering never fetches web previews.

Completed assistant messages trigger bounded metadata checks against the executors
attached at completion: 10 distinct file destinations, 3 seconds per file and
10 seconds overall. Persist results and exact executor identities once, never
recheck on browser reconnect, duplicate completion or daemon restart. File popups
show each executor, resolved path, creation/modification time, byte size, and
Checked timestamp with a live relative age. Distinguish not found, timeout, error,
and not checked. Unavailable/replaced executors are greyed out with an explanation.
Icon-only Show/Download controls have accessible names and tooltips. Both fetch
current contents explicitly; previews support text and raw/pretty JSON, while
binary files can be downloaded. HTML files are inert source text; SVG files use the same bounded local SVG widget as fenced drawings. Complete
reads are limited to 4 MiB; long previews are shortened visibly. Download within
a preview uses its already-fetched bytes. File contents are never cached or added
to the conversation. Popups close via ×, Escape or outside click. Math uses a bounded,
allowlisted MathML tree and retains unsupported source. Mermaid fenced blocks
render with a pinned local bundle inside an opaque-origin sandbox with no network
access; retain source and a readable fallback. The PWA caches the renderer assets
with the release so diagrams work without a CDN.

Each message header shows its first durable Demodex recording time in the browser's
local timezone, with full date/time on hover. Keep that time through streaming,
completion, steering confirmation and reconnects. Codex's imported message items
lack original timestamps: label their snapshot time Imported, rather than showing
it as the original send time. Missing timestamps remain explicitly unknown.


## Explicit notifications

Agents may call `demodex.notify(message, title?)` deliberately. Every call records a
standalone chat notification, even with push disabled or unavailable; never infer
notifications from completion, questions, errors, or subagent events. Duplicate tool
calls return the original receipt without repeating the chat event or push attempt.

Server settings offers Enable push on this device, Disable push, Send test notification,
and Hide message previews. Permission and subscription require an explicit gesture.
The first version binds one daemon to each PWA installation, clearly labelled; users
can disable that subscription before enabling another server. Switching ordinary
connections does not change the notification binding. Distinct server origins can
have their own installations. iOS/iPadOS requires Home Screen installation.

VAPID identity and subscriptions persist in the private daemon database. ECE encrypts
payloads and ES256 signs VAPID claims; HTTPS requests go only to supported browser push
providers, with redirects disabled. Attempts are bounded and never replayed after an
uncertain outcome; 404/410 removes an expired subscription. Delivery status distinguishes
push-service acceptance from unavailable or failed delivery, never claims device display
or user receipt. Notifications click through to the correct server/session without
credentials in URLs. Notification routing preserves other-server drafts. Hidden previews
omit session identity and message content from the OS notification.

`demodex.notify` is registered for new Codex threads; the current Codex resume API
preserves older threads' original tool catalogue. Real push-provider/device acceptance
is an explicit deployment smoke test, not performed by fixture tests.

SVG fenced blocks and explicitly opened SVG files use a deliberately small local drawing subset. Geometry, groups,
text, title and description elements become an allowlisted Yew SVG tree; raw HTML
insertion is never used. Source CSS, resource references, images, links, definitions,
animations and foreign content are unsupported. Any unsupported element or attribute
rejects the whole preview, with a readable reason and exact source available.
Accepted drawings appear in a bounded thumbnail button; clicking opens a larger
modal with Close, Escape and outside-click dismissal. Preview rendering never fetches
resources. Limit input to 128 KiB, 2,048 XML nodes, 32 nesting levels and 16 KiB per
attribute; numeric values must be finite and at most 1,000,000 in magnitude. A
transform attribute accepts at most 16 operations. Require a valid viewBox or positive
numeric width and height. Fill/stroke accept hex and a limited set of color names;
fonts are generic serif, sans-serif or monospace. Raw source remains independently
expandable for both supported and unsupported drawings.

Popup sizing uses the shared Modal component: content-sized forms grow to the
viewport with 32px margins on desktop and 8px on mobile, then scroll internally.
Dynamic directory browsers opt into stable sizing, with a scrolling results area
between fixed navigation and selection controls. Keep these rules shared rather
than adding dialog-specific height caps.
New Session groups have a little extra space above their headings. Resume search
sits beside its checkbox. The host directory sits directly below its checkbox;
unchecking removes it. Other executor removal uses a labelled ×, and creation
buttons follow the existing executor entries. The Sessions tree/list toggle saves
its view preference; flat mode preserves project-scoped reorder constraints.
Archive restore uses a labelled return-arrow icon with the same corner geometry.

The shared `ui::Group` owns settings chrome: a thin rectangular border, dark
inset background, 10px padding and 10px separation, with an optional SectionTitle.
Use it for Server Settings, session controls, new/resumed session field groups,
connection/authentication and executor editors, and bounded metadata/activity
panels. Keep form submission and disabled fieldsets with their owning forms.
Group framing must not depend on being inside Server Settings. Nested headings
use ordinary text; the orange accent identifies the group's main heading.
All primary buttons share hover, pressed and keyboard-focus feedback; disabled
buttons retain their disabled appearance. Server settings retains touch-sized
controls for coarse pointers. Both directory and saved-session browsers use
stable popup sizing with independent scrolling results.

Consistency review covers server settings, connections, creation/resume and
executor editors, session controls/model/goals, session tree/list/archive,
background terminals, directory/search pickers, chat/composer, approvals/questions,
and web/file/SVG previews. Conversation content and pending decisions retain their
purpose-specific structure; shared controls, focus and popup rules still apply.

Folder groups with sessions place a thin shared dashed + Session button directly
below the path, before session cards. It opens the New Session form.
The usage popup shows every reported quota window with relative and absolute reset
times, plus reported credit balances or an explicit unavailable state.

The composer action row always includes the model picker. Selection shares the
Session controls draft and requires Apply model; active sessions keep the picker
visible but disabled. Load the catalogue when the selected session connects.

`ui::Form` keeps each dialog's submit actions in a footer outside its scrollable
fields. Independent sections in Session controls keep their own action rows.
Validation reports errors beside the field, focuses the first invalid field on
submit, and never sends the operation until all enabled fields in that form pass.
Nested dialogs cannot submit their parent form. Operational blockers (busy,
disconnected, active session, unsupported permissions) remain separate from field
validation. Use `ui::Input` for names, paths, required fields, URLs, ports and
numeric limits, with stable accessible labels and error descriptions.

Tree and List are explicit mutually exclusive buttons, with the selected state
exposed to keyboard and assistive technology. `ui::IconButton` owns icon size,
accessible name, tooltip, and hover/pressed/focus feedback. Archive uses the red
destructive variant; Restore and removing an unsaved executor selection are
neutral. Layout-specific CSS positions buttons without redefining their states.

System shares open a compact intake picker with the exact received text and file
names, a New session action and connected, non-archived sessions on the current
server. Offer Choose server without losing the incoming share. The normal New
Session form supplies execution settings; closing it returns to the share picker.
Selection appends original text and quoted uploaded file paths to that session's
draft, preserving existing edits and leaving Send explicit. Keep files unchanged,
including HEIC, and show a large-upload warning above 4 MiB. Interrupted uploads
show unconfirmed receipts and never restart automatically. Discard clears the
local incoming payload; confirmed executor uploads follow existing retention.
