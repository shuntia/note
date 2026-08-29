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
ExecStart=/home/shuntia/Projects/note/target/release/note-server
WorkingDirectory=/home/shuntia/Projects/note
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

Install to `/etc/systemd/system/note.service`, then:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now note
```
