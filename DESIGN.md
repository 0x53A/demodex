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
- Follow new transcript output while the reader is near the bottom; preserve
  their position when they scroll up. Keep the scroll region keyboard accessible.
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
