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
- **Notes list** takes `pointer-events: auto`: the face layer is
  `pointer-events: none` apart from its buttons, so without it the ticks, the
  add input and the menu never received a real click.
- **Start hint on the resting face** stays on one line, so it clears the notes
  list below the small ring.
- **Notes list** keeps a 24 px gutter each side on the phone.

## Checked in a browser (fresh database, not a production copy)

- Now: the strip opens on the seeded block; Stop → Undo restores the paused
  session and a focus refetch inside the window does not bring it back; idle
  in a session hides everything but the arc, the counter and the session's name and step, and the first touch
  only wakes the face. Phone and desktop.
- Inbox: list, detail and the spinning Refresh at both widths; the signal
  file receives the stamp.
- Notes: add, tick, Undo (server reads the note open again), the Edit / Pin /
  Done menu, and a tap on the ring still starts a session.
- Idle nudges: the Settings row reads `20 min`, saves `Off` and keeps it
  after a reload.
