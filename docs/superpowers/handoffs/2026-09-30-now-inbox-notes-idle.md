# Handoff: the Now face, Inbox, Notes and Idle nudges

All four plans from `docs/superpowers/plans/2026-09-30-*.md` are built on
branch `claude/pensive-ptolemy-cfkqkn`, on top of the voice-calls merge.

## Migrations

The database migrates v42 → v45 at start:

| Version | Plan | Change |
|---|---|---|
| v43 | Inbox | `inbox_items` |
| v44 | Notes | `notes` |
| v45 | Idle nudges | `users.last_active_at`; `events.origin` accepts `idle`; `events.cancel_if` accepts `active` |

## Left for the host (Inbox, Task 7)

Refresh writes `/run/note/inbox-refresh`. Until the host watches it, the
button writes a file nobody reads. In `~/Documents/configuration-nix`:

- `modules/schoolwork-check.nix`, after `services.schoolwork-check = { … };`:

  ```nix
    systemd.paths.schoolwork-check-refresh = {
      wantedBy = [ "paths.target" ];
      pathConfig = {
        PathModified = "/run/note/inbox-refresh";
        Unit = "schoolwork-check.service";
      };
    };
  ```

- `modules/note.nix`, inside `services.note.settings`:

  ```nix
        inbox.refresh_signal = "/run/note/inbox-refresh";
  ```

Then `nix flake update note && sudo nixos-rebuild switch`. `RuntimeDirectory =
"note"` already comes from the voice-calls module change.

## Deviations from the plans

- **Inbox routes** call `axum::routing::get`/`post` by path, because
  `inbox::get` (the data function) shares the name.
- **Notes** live on the Notes tab (the former Tasks tab), above the tasks,
  per `specs/2026-09-30-quiet-today-notes-tab-briefs-i18n-design.md`. Today's
  resting face is the full circle again.

## Checked in a browser (fresh database, not a production copy)

- Now: the strip opens on the seeded block; Stop → Undo restores the paused
  session and a focus refetch inside the window does not bring it back; idle
  in a session hides everything but the arc, the counter and the session's name and step, and the first touch
  only wakes the face. Phone and desktop.
- Inbox: list, detail and the spinning Refresh at both widths; the signal
  file receives the stamp.
- Notes: add, tick, Undo (server reads the note open again) and the Edit / Pin /
  Done menu, on the Notes tab.
- Idle nudges: the Settings row reads `20 min`, saves `Off` and keeps it
  after a reload.

## Second round: a quieter Today, briefs, i18n, clippy

Built to `specs/2026-09-30-quiet-today-notes-tab-briefs-i18n-design.md`.

- **Today** rests on the full circle, so the scroll morph carries its wait,
  name and span into the header and hero again.
- **Phone bar**: icons only, Notes · Chat · Today · Memory · Settings.
- **Steps**: the face names the current step, with its task faint beneath
  until idle; the arc is one span.
- **Toasts**: a frosted pill under the top bar (above the tab bar on a phone),
  in and out over 200 ms.
- **Briefs**: before `morning_until` (Settings → Day → Morning ends, default
  11:00) with nothing on for 90 minutes, the day's letter and then the week's
  review take the circle's place; the check marks it read and the circle comes
  back. Scrolling fades the letter and brings the header or hero in.
- **i18n**: `web/src/i18n` holds `t()`, the English dictionary and the
  formatters. `i18n/strings.test.ts` fails on any reader-facing text in the
  covered files; Settings, Calendar, Talk, receipts, Share and Admin are the
  second pass.
- **Clippy**: `clippy::pedantic` is on for the workspace (four lints allowed,
  reasons in `Cargo.toml`). `cargo clippy --workspace --all-targets -- -D
  warnings` is clean; the flake's checks do not run it yet.

Checked in a browser at 390×844 and 1440×900: the letter in place of the circle,
read and back; the scroll fade over the letter; a two-step session (one arc,
parent line gone when idle); a toast over the face; the Notes tab and its menu.
