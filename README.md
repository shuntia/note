# Note

A self-hosted daily planning server: tasks, a generated daily plan of events,
and channel delivery driven by per-user templates.

## Prerequisites

Either:

- A host Rust toolchain via [rustup](https://rustup.rs) (stable, 2021
  edition), or
- `nix develop` from the repo root, which drops you into a shell with
  `cargo`, `rustc`, `rustfmt`, `clippy`, and `sqlite` already on `PATH`.

## Build

```sh
cargo build --release
```

The binary is written to `target/release/note-server`.

## First run

1. Copy or edit `config/server.toml` and `config/defaults/` to taste (the
   checked-in versions work as-is for a local instance).
2. Create the first admin user:

   ```sh
   cargo run --release -- create-user <name> <password> --admin
   ```

3. Start the server:

   ```sh
   cargo run --release
   ```

   It binds to `bind_addr` from `config/server.toml` (default
   `127.0.0.1:3271`) and stores its SQLite database under `data_dir`
   (default `./data`, already gitignored).

`NOTE_CONFIG_DIR` overrides the config directory (default `./config`).

## Config layout

```
config/
  server.toml                       # bind_addr, public_base_url, data_dir
  defaults/
    user.toml                       # display_name, timezone, template
    templates/
      default.toml                  # default event template
  users/
    <username>/
      user.toml                     # per-user overrides, merged over defaults
      templates/
        <name>.toml                 # per-user template overrides
```

Per-user files are optional; anything not overridden falls back to the
`defaults/` tree.

## Memory & agent tools

Per-user long-term memory lives under `data/memory/<user>/{semantic,episodic,procedural,archive}/` —
one markdown fact per file, frontmatter with a one-line summary. Facts are
never deleted: superseding a fact writes a replacement and moves the old file
to `archive/`. A SQLite FTS index over these files is derived and rebuilt at
startup, so the files themselves are the backup-worthy source of truth.

The standing context document each agent session sees is
`config/users/<user>/standing.md`; agents edit it in place through the
`context_edit` tool, so its history is whatever your config dir's VCS says.

Model-facing capabilities are typed tool calls dispatched through a
per-session-type registry (check-in < talk < nightly). Every call is
validated, size-capped, and transactional; failures return typed rejections
to the model and never leave partial state.

Event scheduling semantics: `fixed` events cannot move; `slide` events can be
slid within ±`slide_window_min` minutes of their template time (0 = unbounded);
`drop` events can additionally be dropped by the agent. Snoozing is separate:
any undecided event can be snoozed ("not now"), which re-fires it later and is
not bounded by the slide window.

## Admin API

Admin-role users (see `create-user --admin`) get two extra routes:

- `GET /api/admin/log?limit=100` — the last N `event_log` rows.
- `POST /api/admin/users {username, password, admin}` — create a user.

Both return `403 Forbidden` for non-admin users.

## Running as a systemd service

```ini
[Unit]
Description=Note server
After=network.target

[Service]
ExecStart=~/Projects/note/target/release/note-server
WorkingDirectory=~/Projects/note
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

Install to `/etc/systemd/system/note.service`, then:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now note
```
