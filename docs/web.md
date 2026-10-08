# Web and desktop

## The PWA

`web/` is an installable PWA: React + Vite + TypeScript, UI in English and
Japanese (`web/src/i18n/`). Runtime dependencies (React, React DOM, marked,
DOMPurify, gsap, qrcode) are bundled; the client makes no external requests.

```sh
cd web && pnpm install && pnpm build
```

The server serves the build from `web_dir` (default `web/dist`, resolved
against the working directory) with an SPA fallback; without a build the API
still runs. Unknown `/api/*` paths stay `404`. Development, tests and
screenshots: [development.md](development.md#web-development).

Views (`web/src/views/`):

- **Home**: one face on a gradient. In a focus session it is a gauge around
  the step's remaining time with pause and "Done with this step"; between
  sessions it is the next routine, counted down over an arc. Now offers today's
  run order first, then the queue (`GET /api/tasks/candidates`). Swipe up, scroll
  or ArrowDown raises **Today** under it: the next event with its actions
  (Start, Later, drop, move to tomorrow, silence), and the day drawn as a line.
  A phone opens on Home; a desktop opens on Today.
- **Tasks**: quick-add, steps, durations, and a start that opens a session.
  The soon list leads with today's run order; a long press lifts a row and
  dragging it into, within or out of the order saves it (`PUT /api/order`).
  Later is folded beneath.
- **Calendar**: commitments and quiet windows.
- **Chat** (`Talk.tsx`): persisted conversations, replies as sanitized
  markdown, and one quiet line for what the agent did. With the field empty
  and a voice service configured, the send button is a mic that calls Note. While a reply is coming
  it reads "Note is thinking…" with the tool in flight; after, a header ("Note
  thought for 4 seconds") opens the reasoning and every call. A turn with
  neither reasoning nor tool calls has no header.
- **Memory**: every fact the agent saved (filter, search, open) and the inbox
  of imported items, with Refresh when `[inbox] refresh_signal` is set.
- **Settings**: profile, language, day and template, check-ins, sessions,
  connections (Matrix, calls, voice), Web Push, theme, About you, API tokens,
  share links, security, Debug (the prompt a session starts with), and the admin panel for admins.
- **Onboarding**, **Join** (invite links), **Share** (the `/s/<token>` visitor
  chat), **Admin**.

**The call view** (`web/src/call/`) takes the whole screen: one circle that
breathes while listening, ripples with either voice, and writes while Note
thinks, with the caller's words as captions. While Note runs a tool the rim grows
teeth and turns like a gear; when it lands, a check mark is written inside the
circle, or a red X stroke by stroke when it failed, and erased from where it
began. A tap mutes (or answers a ring), a
downward swipe or Escape ends the call; Note's last words play out first.
Capture and playback run in audio worklets (16 kHz up, jitter-buffered
playback down) over `/api/call/ws` ([voice.md](voice.md#web-calls)). An
`incoming` ring opens the view ringing in a visible tab
([delivery.md](delivery.md#ringing)).

The top bar on desktop holds the views and a quick-capture field (`N`); phones
get a fixed tab bar; a running session takes the whole screen. Deliveries
arrive live over the WebSocket while the app is open and as push notifications
when not; the socket also tells the server whether the page is in view.
`web/public/sw.js` renders push notifications and focuses an open tab on click.

## Desktop

`desktop/` is an Electron shell around the deployed web app: its own window,
a tray icon that shows or hides it, a "Start at login" option, English or
Japanese menus, and `offline.html` when the server cannot be reached.

The server address is the first of `--url=`, `NOTE_URL`, the address saved
in the app (`config.json` in its user data directory) and the `noteUrl` baked
into the build. With none, a small window asks for it ("Server address",
Connect); it takes an http(s) address that answers `GET /api/me` (a 401
counts) or its root, and saves it. "Change server…" in the File menu and the
tray reopens that window; connecting reloads the app on the new server.

```sh
cd desktop && pnpm install
pnpm start          # the resolved address, or the first-run window
pnpm dev            # --dev against http://localhost:5173
pnpm test           # address resolution and update eligibility (node --test)
pnpm dist -c.extraMetadata.noteUrl=https://note.example.com   # AppImage and deb
```

`nix build .#note-desktop` builds it with the flake;
`note-desktop.override { url = "https://note.example.com"; }` bakes the
address in. On NixOS or Home Manager use `programs.note-desktop` instead (see
[deployment.md](deployment.md#desktop-app)).

CI (`.github/workflows/desktop.yml`) builds Linux (AppImage, deb), macOS (dmg,
zip; x64 and arm64) and Windows (nsis) on every change under `desktop/` and
uploads them as run artifacts (`note-desktop-linux`, `-macos`, `-windows`).
A `v*` tag publishes them, with electron-updater's `latest*.yml` and
blockmaps, as the GitHub release of that version. A daily schedule (or a
manual run) publishes `v<package version>-nightly.<YYYYMMDD>` as a prerelease
when `main` has moved since the last nightly, and deletes nightlies older than
14 days. The builds are unsigned: macOS needs right-click → Open the first
time, and Windows shows a SmartScreen prompt.

The Windows installer and the AppImage update themselves: they check GitHub
at start and every 4 hours, download in the background, and offer "Restart to
update" in the tray and File menu once ready ("Check for updates" checks now).
"Nightly builds" there (saved in `config.json`) follows prereleases too. macOS
and the deb get "Check for updates", which opens the releases page; dev runs
and the Nix package never update.

Closing the window hides it to the tray, where the app keeps its socket and
timers running and is still rung: a ring shows, restores and focuses the
window (flashing it, or bouncing the macOS dock icon) and the ringtone plays
without a click. The microphone is granted only to the Note server's own
pages. On macOS the first call asks for microphone access.
