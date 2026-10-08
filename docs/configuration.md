# Configuration

## Layout

```
config/
  server.toml                 # server-wide settings
  defaults/
    user.toml                 # default user settings
    templates/default.toml    # default day template
    prompts/                  # agent prompts (English), prompts/ja/ for Japanese
    memory/{en,ja}/           # built-in facts seeded into every user's memory
  users/<username>/
    user.toml                 # per-user overrides, merged over defaults/user.toml
    templates/<name>.toml     # per-user templates
    prompts/<name>.md         # per-user prompt overrides
    standing.md               # the standing context document (see agent.md)
    nightly_notes.md          # last night's brief to today's sessions
```

Per-user files are optional; anything not overridden falls back to
`defaults/`.

Environment:

- `NOTE_CONFIG_DIR`: the config directory (default `./config`).
- `NOTE_SERVER_CONFIG`: a `server.toml` to read instead of the one in it.
- `NOTE_DEFAULTS_DIR`: the shipped `defaults/` tree (the Nix package carries
  one under `share/note/defaults`). `users/` stays under the config directory,
  since the server writes it.

Relative paths in `server.toml` (`data_dir`, `web_dir`, key files) resolve
against the server's working directory.

## server.toml

`config/server.toml` is the annotated reference. Sections:

| Key / section | Purpose |
|---|---|
| `bind_addr` | default `127.0.0.1:3271` |
| `public_base_url` | an `https://` URL turns on `Secure` cookies and passkeys |
| `data_dir` | database and memory files; default `data` |
| `web_dir` | the built PWA; default `web/dist`; missing means API only |
| `secrets_dir` | default `persist/secrets`; holds the legacy `admin_totp` seed |
| `[admin]` | `require_second_factor`, `rp_id`, `rp_origin` ([security.md](security.md)) |
| `[providers.llm]`, `[providers.embeddings]` | model providers ([agent.md](agent.md#providers)) |
| `[search]` | `searxng_url`, `max_results` (8), `timeout_secs` (15) ([agent.md](agent.md#web-search)) |
| `[agent]` | `idle_summary_min` (60): a conversation quiet this long is summarised; `0` turns it off |
| `[inbox]` | `refresh_signal`: a file the Memory tab's Refresh writes the time to; the host watches it and runs the sync. Unset hides the button |
| `[limits]` | spend ceilings, below |
| `[channels.webpush]`, `[channels.matrix]` | delivery channels ([delivery.md](delivery.md)) |
| `[voice]` | the voice-service link ([voice.md](voice.md)) |

`[limits]`, every key optional:

| Key | Default | |
|---|---|---|
| `agent_sessions_per_day` | 200 | agent sessions per user in any 24 hours; `0` lifts the ceiling |
| `share_max_days` | 120 | farthest a share link may run; 1 to 36500 |
| `share_messages_per_day` | 100 | ceiling a link's own daily cap may reach; at least 1 |
| `shares_per_user` | 20 | `0` turns share links off |
| `share_distant_km` | 300 | a share visit farther than this from the owner is marked |

## user.toml and the settings API

`config/users/<username>/user.toml` is what the settings routes read and write,
so a hand-edited file and a client-side change are the same thing.

`GET /api/settings` returns the effective values (defaults merged with the
user's file), plus `category`, `templates` (every `.toml` stem under
`defaults/templates/` and the user's own), `timezones` (the bundled IANA
database), `schedule` (the template's routines), `voice_enabled`,
`voice_link` and `matrix_enabled`.

`PUT /api/settings` takes any subset of the keys below and returns the merged
settings. A rejected value is a `400` whose `{"error"}` names
the field and leaves the file untouched. The write goes through a temp file and
a rename. A toggle the user never set stays out of the file and follows its
category default.

| Key | Constraint |
|---|---|
| `display_name` | trimmed, non-blank, at most 64 characters |
| `timezone` | an IANA name |
| `timezone_auto` | bool |
| `language` | blank, `en` or `ja`; changing it resets `voice_voice` unless one is sent too |
| `template` | one of `templates` |
| `nightly_time` | zero-padded 24-hour `HH:MM` (default `03:00`) |
| `close_day_time` | `HH:MM`, or blank for no close of day |
| `morning_until` | `HH:MM` |
| `show_arc_between_sessions` | bool: whether the wait between sessions draws its arc |
| `counter` | `remaining` or `elapsed` |
| `nightly_enabled`, `checkins_enabled` | bool; default from the category |
| `triggers_per_day` | 0 to 20 check-ins Note may start on its own |
| `pomodoro_enabled` | bool |
| `pomodoro_work_min` | 5 to 120 |
| `pomodoro_break_min` | 1 to 60 |
| `session_end_notify` | bool |
| `idle_nudge_min` | 0 to 240 |
| `ring_for` | `urgent`, `checkins` or `never` |
| `voice_voice` | a call voice id; blank for the language default |
| `voice_cue` | bool (default true) |
| `matrix_send`, `matrix_ping` | bool |
| `alerts` | `[{index, alert}]`: which of the template's routines ping |

`category` is read-only here; it belongs to the account.

## Account categories

Every account has a `category` beside its role: `member`, or `test` for one
that exists to exercise the API. The category decides where the two background
features start: the nightly run (the day's plan and its debrief) and check-ins.
Both spend tokens or attention unasked, so a `test` account starts with both off
and a `member` with both on.

`nightly_enabled` and `checkins_enabled` override that either way, live: a
disabled user is skipped by the nightly and delivery sweeps without a model call
or a log row, and switching back on takes effect on the next sweep. Talk is
never gated.

```sh
note-server create-user aitest <password> --test
note-server set-category aitest test     # move an existing account
```
