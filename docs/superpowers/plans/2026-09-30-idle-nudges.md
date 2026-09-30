# Idle Nudges Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When the user has gone quiet with open notes, Note lays an idle trigger. The trigger session sees the notes and either nudges about one or stays quiet. Coming back calls a pending nudge off.

**Architecture:** A new `presence` module keeps `users.last_active_at`, stamping it at most once a minute. The stamp comes from session writes, `POST /api/presence`, client socket messages, chat turns and Telegram buttons. A new `idle` module runs on every runner sweep. It lays a `trigger` event with `origin = 'idle'` and `cancel_if = 'active'` through `triggers::insert`, and that event counts against the day's budget. The existing trigger path fires it. For an idle trigger, `triggers::fire` appends the idle note to the opening. After `say`, it stamps `notes.last_nudged_at` on the note ids that `say` names.

**Tech Stack:** Rust (axum 0.8, rusqlite 0.40 bundled, jiff 0.2), React 19 + TypeScript, vitest.

**Spec:** `docs/superpowers/specs/2026-09-30-postits-inbox-focus-connections-design.md`, section "4. Idle nudges".

**Depends on:** the Notes plan (`docs/superpowers/plans/2026-09-30-notes.md`, group 3) must already be merged into the branch. It creates table `notes(id, user_id, text, pinned, created_at, done_at, last_nudged_at)`, where `done_at IS NULL` means open, `pinned` is 0/1, and timestamps are RFC 3339 UTC text. It also adds the `/api/notes` routes. This plan reads and stamps that table and never creates it.

## Global Constraints

- **Comments:** follow the CLAUDE.md comment policy.
  - Comment only at a declaration, and only when the signature cannot say it.
  - Code that reads clearly on a first pass gets no comment.
  - No narration of process history, and no statements about wrong or disproven approaches.
- **UI taste:** as little text as possible. The new Settings row carries a label and a value, and nothing else.
- **Commits:** every commit message ends with these two lines:
  ```
  Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01LZnmd9SLioNADcyRtSoFuK
  ```
- **Cargo:** always run with `CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/<branch>`, where `<branch>` is the execution branch. The commands below write it as `idle-nudges`.
- **Web checks:** `pnpm -C web build && pnpm -C web test`.
- **No new dependencies:** none in cargo, none in npm.
- **Migration number:** the migration goes at the end of `MIGRATIONS` in `server/src/db.rs`. Its number is whatever comes next when the plan is executed; main's last is v41, and the Notes migration and other branches add more. Label it `// v<N>` with that number. It must be the last entry when its test is written.
- **Presence stamps:** whole-second RFC 3339 UTC (`2026-09-30T12:00:00Z`), so they compare exactly as text.
- **Idle trigger shape:** `events.kind = 'trigger'`, `origin = 'idle'`, `cancel_if = 'active'`, `work_session_id = NULL`, `conversation_id = NULL`. It counts against `triggers::spent` like an `agent` trigger.

## Review Focus

1. **Background traffic is not the user.** An open tab polls `GET /api/day` every 60 s, and the browser answers socket pings on its own; an API-token script writes tasks. None of this may stamp presence, or idle never arrives. Pinned by Task 2 (reads and token writes) and Task 3 (pong frames).
2. **Overnight silence.** A user last seen at 22:00 must not be nudged at 00:00 because the new date's budget is fresh. The idle day starts when the user shows up that local day. Pinned by Task 6, `a_user_not_seen_today_is_left_alone`.
3. **One idle stretch gives one nudge per threshold.** After a nudge or a quiet decision, the next check must not lay another trigger a minute later. The idle clock restarts at the last idle trigger. Pinned by Task 6, `the_idle_clock_restarts_at_the_last_idle_trigger`, and Task 7, `a_nudge_held_back_stays_held_until_another_stretch_of_quiet`.
4. **Coming back between lay and fire.** A pending idle trigger is dropped without running a session, even when the presence stamp falls in the same second the trigger was laid. Pinned by Task 5, `activity_after_an_idle_trigger_was_laid_cancels_it`, and Task 7, `coming_back_before_the_nudge_fires_calls_it_off`.
5. **A nudge naming the wrong note.** Ids in `say`'s `notes` that belong to another user, or name a done note, are ignored. Pinned by Task 6, `a_nudge_stamps_only_the_users_own_open_notes`.

## Rulings on spec ambiguities

- **Which API calls count.** Only session-cookie writes count: any method that is not GET, HEAD, OPTIONS or TRACE, made through `CurrentUser`. Reads and API-token calls do not count.
- **WebSocket.** A client-sent text or binary frame counts; ping and pong control frames do not. The web client signals presence through `POST /api/presence`.
- **Chat, Telegram and Matrix.** `talk::run_turn` stamps presence, which covers web chat, Telegram and any later channel that runs turns through it, Matrix included. Telegram button presses stamp as well. The hook point for Matrix is the doc line on `presence::touch`.
- **Idle start.** The idle clock is `max(last_active_at, created_at of the user's last idle trigger)`. A user with no `last_active_at`, or none on today's local date, is not idle.
- **Close of day.** A blank `close_day_time` means there is no cutoff.
- **Check-ins.** A user whose check-ins are off (`features.checkins == false`) gets no idle triggers, the same as the runner never firing theirs.
- **Pending.** An idle trigger in status `pending`, `snoozed` or `fired` counts as pending.
- **"Per minute".** The check runs on the runner's existing 30 s sweep, before `fire_due`, so a trigger laid now fires in the same sweep. Its own conditions keep it to one trigger at a time.
- **Which notes a nudge names.** `say` gains an optional `notes: [id]` field. Only an idle firing reads it.
- **Buttons.** An idle nudge's notification carries no event buttons. Done, snooze and drop act on events, not notes.
- **`idle_nudge_min` range.** 0 to 240.

---

## File Structure

| File | Responsibility |
|---|---|
| `server/src/presence.rs` (new) | `stamp`, `touch` (throttled `last_active_at` write), `last_active` |
| `server/src/idle.rs` (new) | `ORIGIN`, `PROMPT`, `check` (the per-sweep decision and lay), `context` (idle note), `stamp_nudged` |
| `server/src/lib.rs` | `pub mod idle; pub mod presence;` |
| `server/src/db.rs` | migration: `users.last_active_at`; `events.origin` gains `idle`; `events.cancel_if` gains `active` |
| `server/src/auth.rs` | `CurrentUser` extractor stamps presence on writes |
| `server/src/api.rs` | `POST /api/presence`; `ws_pump` stamps on client frames; `idle_nudge_min` in settings |
| `server/src/channels/ws.rs` | `is_presence(&Message)` |
| `server/src/talk.rs` | `run_turn` stamps presence |
| `server/src/telegram.rs` | `receive_callback` stamps presence |
| `server/src/config.rs` | `idle_nudge_min` field, `DEFAULT_IDLE_NUDGE_MIN`, accessor |
| `server/src/triggers.rs` | `Cancel::Active`; `spent` counts `idle`; `Firing.origin`; `cancelled` handles `active`; `fire` adds the idle note, stamps notes, drops buttons |
| `server/src/tools/trigger_ops.rs` | `SayArgs.notes` |
| `server/src/runner.rs` | `sweep_once` calls `idle::check` before `fire_due` |
| `config/defaults/prompts/trigger.md` | idle section |
| `server/tests/presence_api.rs` (new) | presence over HTTP |
| `server/tests/idle_api.rs` (new) | idle nudge end to end through `sweep_once` |
| `server/tests/telegram_api.rs`, `server/tests/settings_api.rs` | one test each |
| `web/src/presence.ts` + `presence.test.ts` (new) | `startPresence` |
| `web/src/app.tsx` | starts presence while signed in |
| `web/src/api.ts`, `web/src/types.ts` | `presence()`, `idle_nudge_min`, `EventOrigin` gains `'idle'` |
| `web/src/views/Settings.tsx` | "Nudge when idle" row |

---

### Task 1: Migration and the presence stamp

**Files:**
- Modify: `server/src/db.rs` (append to `MIGRATIONS`, add a test at the end of `mod tests`)
- Create: `server/src/presence.rs`
- Modify: `server/src/lib.rs` (module list, alphabetical)

**Interfaces:**
- Consumes: the `notes` table migration from the Notes plan, which is already in `MIGRATIONS`.
- Produces:
  - `pub fn presence::stamp(at: jiff::Timestamp) -> String`
  - `pub fn presence::touch(conn: &Connection, user_id: i64, now: jiff::Timestamp) -> rusqlite::Result<bool>`
  - `pub fn presence::last_active(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<jiff::Timestamp>>`
  - `pub const presence::EVERY_SECS: i64 = 60`
  - Columns: `users.last_active_at TEXT`. `events.origin` also accepts `'idle'`, and `events.cancel_if` also accepts `'active'`.

- [ ] **Step 1: Write the failing migration test**

Append this inside `mod tests` in `server/src/db.rs`:

```rust
    #[test]
    fn the_idle_migration_opens_origin_and_cancel_if_and_keeps_every_row() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        apply_migrations(&conn, &MIGRATIONS[..MIGRATIONS.len() - 1]).unwrap();
        conn.execute_batch(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member');
             INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-09-30', 'x');
             INSERT INTO events (plan_id, kind, wall_time, origin, cancel_if)
                 VALUES (1, 'trigger', '10:00', 'agent', 'replied');
             INSERT INTO event_tasks (event_id, task_id)
                 SELECT 1, id FROM tasks WHERE 0;",
        )
        .unwrap();
        apply_migrations(&conn, MIGRATIONS).unwrap();

        let kept: (String, Option<String>) = conn
            .query_row("SELECT origin, cancel_if FROM events WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(kept, ("agent".to_string(), Some("replied".to_string())));
        conn.execute("UPDATE events SET origin = 'idle', cancel_if = 'active' WHERE id = 1", [])
            .unwrap();
        assert!(
            conn.execute("UPDATE events SET origin = 'somewhere' WHERE id = 1", []).is_err(),
            "origin is still a closed set"
        );
        assert!(
            conn.execute("UPDATE events SET cancel_if = 'whenever' WHERE id = 1", []).is_err(),
            "the cancel rule is still a closed set"
        );
        let fresh: String = {
            conn.execute("INSERT INTO events (plan_id, kind, wall_time) VALUES (1, 'nudge', '11:00')", [])
                .unwrap();
            conn.query_row("SELECT origin FROM events WHERE id = 2", [], |r| r.get(0)).unwrap()
        };
        assert_eq!(fresh, "template");
        let seen: Option<String> = conn
            .query_row("SELECT last_active_at FROM users WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert!(seen.is_none());
    }
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --lib the_idle_migration_opens_origin`

Expected: FAIL. `UPDATE events SET origin = 'idle'` violates the CHECK constraint, because the last migration is still the Notes one.

- [ ] **Step 3: Add the migration**

Append a new last entry to `MIGRATIONS` in `server/src/db.rs`, with `<N>` set to its number at execution time:

```rust
    // v<N>
    "
    ALTER TABLE users ADD COLUMN last_active_at TEXT;
    ALTER TABLE events ADD COLUMN origin_next TEXT NOT NULL DEFAULT 'template'
        CHECK (origin_next IN ('template','agent','auto','user','idle'));
    UPDATE events SET origin_next = origin;
    ALTER TABLE events DROP COLUMN origin;
    ALTER TABLE events RENAME COLUMN origin_next TO origin;
    ALTER TABLE events ADD COLUMN cancel_if_next TEXT
        CHECK (cancel_if_next IS NULL
               OR cancel_if_next IN ('replied','task_done','event_decided','active'));
    UPDATE events SET cancel_if_next = cancel_if;
    ALTER TABLE events DROP COLUMN cancel_if;
    ALTER TABLE events RENAME COLUMN cancel_if_next TO cancel_if;
    ",
```

A column-level CHECK belongs to its own column, so SQLite allows the `DROP COLUMN`. `RENAME COLUMN` rewrites the CHECK text. Neither column is indexed or used in a view.

- [ ] **Step 4: Run the migration test and the whole db suite**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --lib db::`

Expected: PASS, including `v23_…` and `v27_…`, which still reject `'somewhere'` and `'whenever'`.

- [ ] **Step 5: Write the failing presence tests**

Create `server/src/presence.rs` with only the tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    #[test]
    fn a_stamp_is_whole_seconds() {
        assert_eq!(stamp(at("2026-09-30T12:00:00.123456Z")), "2026-09-30T12:00:00Z");
    }

    #[test]
    fn a_stamp_moves_at_most_once_a_minute() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        assert_eq!(last_active(&conn, uid).unwrap(), None);

        assert!(touch(&conn, uid, at("2026-09-30T12:00:00.700Z")).unwrap());
        assert_eq!(last_active(&conn, uid).unwrap(), Some(at("2026-09-30T12:00:00Z")));
        assert!(!touch(&conn, uid, at("2026-09-30T12:00:59Z")).unwrap());
        assert_eq!(last_active(&conn, uid).unwrap(), Some(at("2026-09-30T12:00:00Z")));
        assert!(touch(&conn, uid, at("2026-09-30T12:01:00Z")).unwrap());
        assert_eq!(last_active(&conn, uid).unwrap(), Some(at("2026-09-30T12:01:00Z")));
    }

    #[test]
    fn one_users_stamp_leaves_another_alone() {
        let conn = crate::db::open_memory().unwrap();
        let aki = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        let bo = crate::auth::create_user(&conn, "bo", "p", false).unwrap();
        touch(&conn, aki, at("2026-09-30T12:00:00Z")).unwrap();
        assert_eq!(last_active(&conn, bo).unwrap(), None);
    }
}
```

Add `pub mod presence;` to `server/src/lib.rs`, between `pub mod plan;` and `pub mod prompts;`.

- [ ] **Step 6: Run the tests and confirm they fail**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --lib presence::`

Expected: FAIL to compile, because `stamp`, `touch` and `last_active` are not found.

- [ ] **Step 7: Implement presence**

Put this at the top of `server/src/presence.rs`, above the tests:

```rust
use rusqlite::{Connection, OptionalExtension};

pub const EVERY_SECS: i64 = 60;

/// Whole seconds, so two stamps compare exactly as text.
pub fn stamp(at: jiff::Timestamp) -> String {
    jiff::Timestamp::from_second(at.as_second())
        .expect("a timestamp's own second is in range")
        .to_string()
}

/// Records that the user is here: a session write, a message on their socket,
/// or a chat turn on any channel that runs through `talk::run_turn` (web,
/// Telegram, Matrix). Returns whether the stamp moved; it moves at most once
/// every `EVERY_SECS`.
pub fn touch(conn: &Connection, user_id: i64, now: jiff::Timestamp) -> rusqlite::Result<bool> {
    let cutoff = stamp(now - jiff::Span::new().seconds(EVERY_SECS));
    let n = conn.execute(
        "UPDATE users SET last_active_at = ?1
         WHERE id = ?2 AND (last_active_at IS NULL OR last_active_at <= ?3)",
        (stamp(now), user_id, cutoff),
    )?;
    Ok(n > 0)
}

pub fn last_active(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<jiff::Timestamp>> {
    let raw: Option<String> = conn
        .query_row("SELECT last_active_at FROM users WHERE id = ?1", [user_id], |r| r.get(0))
        .optional()?
        .flatten();
    Ok(raw.and_then(|s| s.parse().ok()))
}
```

- [ ] **Step 8: Run the tests and confirm they pass**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --lib presence::`

Expected: PASS (3 tests).

- [ ] **Step 9: Commit**

```bash
git add server/src/db.rs server/src/presence.rs server/src/lib.rs
git commit -m "feat(server): last_active_at, idle origin and active cancel rule

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LZnmd9SLioNADcyRtSoFuK"
```

---

### Task 2: Session writes and `POST /api/presence` stamp presence

**Files:**
- Modify: `server/src/auth.rs` (the `CurrentUser` struct doc and `from_request_parts`)
- Modify: `server/src/api.rs` (the router list near `/api/settings`, plus a new handler)
- Create: `server/tests/presence_api.rs`

**Interfaces:**
- Consumes: `presence::touch` and `presence::last_active` (Task 1).
- Produces: route `POST /api/presence`, which answers 204. Every non-safe request through `CurrentUser` stamps presence.

- [ ] **Step 1: Write the failing integration tests**

Create `server/tests/presence_api.rs`:

```rust
mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use note_server::AppState;
use tower::ServiceExt;

fn seen(state: &AppState) -> Option<String> {
    let conn = state.db();
    conn.query_row("SELECT last_active_at FROM users WHERE id = 1", [], |r| r.get(0)).unwrap()
}

#[tokio::test]
async fn a_presence_ping_stamps_the_user_and_a_read_does_not() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;

    let res = app
        .clone()
        .oneshot(Request::get("/api/me").header(header::COOKIE, &cookie).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(seen(&state).is_none(), "an open page polls; a read is not the user");

    let res = app
        .oneshot(
            Request::post("/api/presence")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let at: jiff::Timestamp = seen(&state).expect("a stamp").parse().unwrap();
    assert!((jiff::Timestamp::now().as_second() - at.as_second()).abs() <= 2);
}

#[tokio::test]
async fn an_api_token_write_is_not_presence() {
    let (app, _cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let secret = {
        let conn = state.db();
        note_server::tokens::create(&conn, 1, "script").unwrap().token
    };
    let res = app
        .oneshot(
            Request::post("/api/tasks")
                .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"title":"from the script"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(res.status().is_success(), "{}", res.status());
    assert!(seen(&state).is_none());
}

#[tokio::test]
async fn presence_needs_a_session() {
    let (app, _cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let res = app
        .oneshot(Request::post("/api/presence").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(seen(&state).is_none());
}
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --test presence_api`

Expected: `a_presence_ping_stamps_the_user_and_a_read_does_not` FAILS with 404 for `/api/presence`. `presence_needs_a_session` also FAILS, since an unknown route answers 404, not 401.

- [ ] **Step 3: Stamp in the extractor**

In `server/src/auth.rs`, change `use axum::http::{request::Parts, StatusCode};` so it stays as it is. The method check uses `parts.method.is_safe()`, which needs no import.

Replace the doc and struct header:

```rust
#[derive(Debug, Clone)]
pub struct CurrentUser {
```

with:

```rust
/// A cookie session. A write through one stamps the user as present; a read
/// does not, because an open page polls.
#[derive(Debug, Clone)]
pub struct CurrentUser {
```

In `from_request_parts`, replace:

```rust
        let expires: jiff::Timestamp = expires_at.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
        if disabled || expires < jiff::Timestamp::now() {
            return Err(StatusCode::UNAUTHORIZED);
        }
```

with:

```rust
        let expires: jiff::Timestamp = expires_at.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
        let now = jiff::Timestamp::now();
        if disabled || expires < now {
            return Err(StatusCode::UNAUTHORIZED);
        }
        if !parts.method.is_safe() {
            let _ = crate::presence::touch(&conn, id, now);
        }
```

- [ ] **Step 4: Add the route**

In `server/src/api.rs` `router`, add this line right after `.route("/api/settings", get(settings_get).put(settings_put))`:

```rust
        .route("/api/presence", post(presence))
```

Add the handler next to `settings_get`:

```rust
/// The extractor has already stamped the user; the route is how a page that
/// only reads says someone is looking at it.
async fn presence(_user: CurrentUser) -> StatusCode {
    StatusCode::NO_CONTENT
}
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --test presence_api --test auth --test tokens_api`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add server/src/auth.rs server/src/api.rs server/tests/presence_api.rs
git commit -m "feat(server): session writes and POST /api/presence stamp presence

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LZnmd9SLioNADcyRtSoFuK"
```

---

### Task 3: Socket messages, chat turns and Telegram buttons stamp presence

**Files:**
- Modify: `server/src/channels/ws.rs` (new fn and test)
- Modify: `server/src/api.rs` (`ws_connect`, `ws_pump`)
- Modify: `server/src/talk.rs` (`run_turn`)
- Modify: `server/src/telegram.rs` (`receive_callback`)
- Test: `server/tests/telegram_api.rs`

**Interfaces:**
- Consumes: `presence::touch` (Task 1).
- Produces: `pub fn channels::ws::is_presence(msg: &axum::extract::ws::Message) -> bool`.

- [ ] **Step 1: Write the failing tests**

Append this inside `mod tests` in `server/src/channels/ws.rs`:

```rust
    #[test]
    fn only_a_frame_the_client_chose_to_send_is_presence() {
        use axum::extract::ws::Message;
        assert!(is_presence(&Message::Text("hi".into())));
        assert!(is_presence(&Message::Binary(Vec::new().into())));
        assert!(!is_presence(&Message::Pong(Vec::new().into())));
        assert!(!is_presence(&Message::Ping(Vec::new().into())));
    }
```

Append this to `server/tests/telegram_api.rs`:

```rust
#[tokio::test]
async fn a_telegram_message_or_button_counts_as_presence() {
    let fake = common::fake_telegram();
    fake.answer(common::GET_ME);
    let (_app, _cookie, state, _cfg) =
        common::app_with_telegram(says(&["the day is yours"]), &fake.base).await;
    fake.call();
    link(&state, 42);
    let seen = |state: &AppState| -> Option<String> {
        let conn = state.db();
        conn.query_row("SELECT last_active_at FROM users WHERE id = 1", [], |r| r.get(0)).unwrap()
    };
    assert!(seen(&state).is_none());

    fake.answer(&updates(&[(11, 42, "how does today look?")]));
    fake.answer(common::SENT);
    let mut chats = note_server::telegram::Chats::default();
    note_server::telegram::poll_once(&state, &mut chats).await.unwrap();
    assert!(seen(&state).is_some(), "a message is the user");

    state.db().execute("UPDATE users SET last_active_at = NULL", []).unwrap();
    let event_id = routine(&state, 1, "2026-01-05");
    fake.answer(&presses(&[(12, 42, "q1", &format!("ev:done:{event_id}"))]));
    for _ in 0..3 {
        fake.answer(ANSWERED);
    }
    note_server::telegram::poll_once(&state, &mut chats).await.unwrap();
    assert!(seen(&state).is_some(), "a button is the user");
}
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --lib channels::ws:: ; CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --test telegram_api a_telegram_message_or_button`

Expected: the ws test fails to compile (`is_presence` not found). The telegram test FAILS at "a message is the user".

- [ ] **Step 3: Implement `is_presence`**

In `server/src/channels/ws.rs`, add this after `impl ClientHub { … broadcast_changed … }`:

```rust
/// A frame the client chose to send. Ping and pong are answered by the
/// browser whether or not anyone is there.
pub fn is_presence(msg: &axum::extract::ws::Message) -> bool {
    use axum::extract::ws::Message;
    matches!(msg, Message::Text(_) | Message::Binary(_))
}
```

- [ ] **Step 4: Stamp from the socket**

In `server/src/api.rs`, change `ws_connect`'s upgrade line to:

```rust
    ws.on_upgrade(move |socket| ws_pump(socket, state.hub.clone(), state.db.clone(), user.id))
```

Change the `ws_pump` signature to:

```rust
async fn ws_pump(
    mut socket: axum::extract::ws::WebSocket,
    hub: std::sync::Arc<crate::channels::ws::ClientHub>,
    db: std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
    user_id: i64,
) {
```

and its inbound arm to:

```rust
            inbound = socket.recv() => match inbound {
                Some(Ok(Message::Close(_))) => break,
                Some(Ok(msg)) => {
                    last_inbound = tokio::time::Instant::now();
                    if crate::channels::ws::is_presence(&msg) {
                        let conn = crate::db_guard(&db);
                        let _ = crate::presence::touch(&conn, user_id, jiff::Timestamp::now());
                    }
                }
                _ => break,
            },
```

- [ ] **Step 5: Stamp from chat turns and Telegram buttons**

In `server/src/talk.rs` `run_turn`, add this directly after the `MAX_MESSAGE` blank check:

```rust
    {
        let conn = state.db();
        let _ = crate::presence::touch(&conn, user_id, jiff::Timestamp::now());
    }
```

In `server/src/telegram.rs` `receive_callback`, change the closure's start from:

```rust
        link_for_chat(&conn, cb.chat_id).unwrap_or(None).and_then(|link| {
            let did = apply(&conn, &state.config_dir, &link, &cb.data, now)?;
```

to:

```rust
        link_for_chat(&conn, cb.chat_id).unwrap_or(None).and_then(|link| {
            let _ = crate::presence::touch(&conn, link.user_id, now);
            let did = apply(&conn, &state.config_dir, &link, &cb.data, now)?;
```

- [ ] **Step 6: Run the tests and confirm they pass**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --lib channels::ws:: && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --test telegram_api --test ws_api --test talk_api`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add server/src/channels/ws.rs server/src/api.rs server/src/talk.rs server/src/telegram.rs server/tests/telegram_api.rs
git commit -m "feat(server): socket frames, chat turns and Telegram buttons stamp presence

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LZnmd9SLioNADcyRtSoFuK"
```

---

### Task 4: The `idle_nudge_min` setting

**Files:**
- Modify: `server/src/config.rs` (`UserConfig` field, const, accessor, test)
- Modify: `server/src/api.rs` (`SettingsPatch`, a range const, `settings_body`, `settings_put`)
- Test: `server/tests/settings_api.rs`

**Interfaces:**
- Produces:
  - `pub const config::DEFAULT_IDLE_NUDGE_MIN: u32 = 20`
  - `pub fn UserConfig::idle_nudge_min(&self) -> u32`
  - Settings JSON gains `"idle_nudge_min": number`, and PUT accepts it in the range 0..=240.

- [ ] **Step 1: Write the failing tests**

Append this inside `mod tests` in `server/src/config.rs`:

```rust
    #[test]
    fn idle_nudges_default_to_twenty_minutes_and_stay_out_of_an_untouched_file() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n");
        let cfg = UserConfig::load(tmp.path(), "aki").unwrap();
        assert_eq!(cfg.idle_nudge_min(), DEFAULT_IDLE_NUDGE_MIN);
        cfg.save(tmp.path(), "aki").unwrap();
        let raw = std::fs::read_to_string(tmp.path().join("users/aki/user.toml")).unwrap();
        assert!(!raw.contains("idle_nudge_min"), "unexpected file: {raw}");

        write(tmp.path(), "users/aki/user.toml", "idle_nudge_min = 0\n");
        assert_eq!(UserConfig::load(tmp.path(), "aki").unwrap().idle_nudge_min(), 0);
    }
```

Append this to `server/tests/settings_api.rs`:

```rust
#[tokio::test]
async fn idle_nudges_are_a_setting_that_zero_turns_off() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let v = json(app.clone().oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["idle_nudge_min"], 20);

    let res = app.clone().oneshot(put(&cookie, r#"{"idle_nudge_min":0}"#)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(json(res).await["idle_nudge_min"], 0);

    let res = app.clone().oneshot(put(&cookie, r#"{"idle_nudge_min":241}"#)).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(res).await["error"], "idle_nudge_min must be 0 to 240");

    let v = json(app.oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["idle_nudge_min"], 0);
}
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --lib idle_nudges_default ; CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --test settings_api idle_nudges`

Expected: the lib test fails to compile. The API test FAILS: `idle_nudge_min` is `null`, and the PUT answers 422 on an unknown field.

- [ ] **Step 3: Implement the config field**

In `server/src/config.rs` `UserConfig`, add this after `session_end_notify`:

```rust
    /// Minutes without a sign of the user before Note may nudge about open
    /// notes; 0 turns idle nudges off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_nudge_min: Option<u32>,
```

Next to the other defaults:

```rust
pub const DEFAULT_IDLE_NUDGE_MIN: u32 = 20;
```

In `impl UserConfig`:

```rust
    pub fn idle_nudge_min(&self) -> u32 {
        self.idle_nudge_min.unwrap_or(DEFAULT_IDLE_NUDGE_MIN)
    }
```

- [ ] **Step 4: Implement the API field**

In `server/src/api.rs`:

`SettingsPatch` gains the field after `session_end_notify`:

```rust
    idle_nudge_min: Option<u32>,
```

Next to `POMODORO_BREAK_MIN`:

```rust
const IDLE_NUDGE_MIN: std::ops::RangeInclusive<u32> = 0..=240;
```

In `settings_body`, add this after `"session_end_notify": …,`:

```rust
        "idle_nudge_min": cfg.idle_nudge_min(),
```

In `settings_put`, add this after the `pomodoro_break_min` block:

```rust
    if let Some(n) = req.idle_nudge_min {
        if !IDLE_NUDGE_MIN.contains(&n) {
            return invalid_field(
                "idle_nudge_min",
                &format!("must be {} to {}", IDLE_NUDGE_MIN.start(), IDLE_NUDGE_MIN.end()),
            );
        }
        cfg.idle_nudge_min = Some(n);
    }
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --lib config:: && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --test settings_api`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add server/src/config.rs server/src/api.rs server/tests/settings_api.rs
git commit -m "feat(server): idle_nudge_min setting

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LZnmd9SLioNADcyRtSoFuK"
```

---

### Task 5: Triggers learn the idle origin and the `Active` cancel rule

**Files:**
- Modify: `server/src/triggers.rs` (`Cancel`, `spent`, `Firing`, `read`, `cancelled`, tests)

**Interfaces:**
- Consumes: `presence::last_active` and `presence::touch` (Task 1).
- Produces:
  - `Cancel::Active`, whose `as_str()` is `"active"` and which has no reference.
  - `Firing.origin: String`.
  - `spent` counts origins `'agent'` and `'idle'`.
  - `cancelled` returns true for `"active"` once `last_active_at` falls in or after the second the trigger was laid.

- [ ] **Step 1: Write the failing tests**

Append this inside `mod tests` in `server/src/triggers.rs`:

```rust
    #[test]
    fn an_idle_trigger_spends_the_days_budget() {
        let (conn, tmp, uid) = env();
        let plan_id = crate::plan::ensure(&conn, tmp.path(), "aki", uid, date()).unwrap();
        insert(&conn, plan_id, "09:00", "idle", "idle", Some(Cancel::Active), None, None,
            at("2026-09-17T09:00:00Z")).unwrap();
        assert_eq!(spent(&conn, uid, date()).unwrap(), 1);
    }

    #[test]
    fn activity_after_an_idle_trigger_was_laid_cancels_it() {
        let (conn, tmp, uid) = env();
        let plan_id = crate::plan::ensure(&conn, tmp.path(), "aki", uid, date()).unwrap();
        let id = insert(&conn, plan_id, "09:00", "idle", "idle", Some(Cancel::Active), None, None,
            at("2026-09-17T09:00:00.400Z")).unwrap();
        let ev = read(&conn, id).unwrap().unwrap();
        assert_eq!(ev.origin, "idle");
        assert_eq!(ev.cancel_if.as_deref(), Some("active"));

        conn.execute("UPDATE users SET last_active_at = '2026-09-17T08:30:00Z' WHERE id = ?1", [uid])
            .unwrap();
        assert!(!cancelled(&conn, uid, &ev).unwrap(), "the quiet before it is what laid it");

        crate::presence::touch(&conn, uid, at("2026-09-17T09:00:00.900Z")).unwrap();
        assert!(cancelled(&conn, uid, &ev).unwrap(), "a stamp in the same second still counts");
    }
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --lib triggers::tests`

Expected: fails to compile, because `Cancel::Active` and `Firing.origin` do not exist.

- [ ] **Step 3: Implement**

In `server/src/triggers.rs`:

Change the `Cancel` enum and impl to:

```rust
#[derive(Clone, Copy, Debug)]
pub enum Cancel {
    Replied,
    TaskDone(i64),
    EventDecided(i64),
    Active,
}

impl Cancel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Cancel::Replied => "replied",
            Cancel::TaskDone(_) => "task_done",
            Cancel::EventDecided(_) => "event_decided",
            Cancel::Active => "active",
        }
    }

    fn reference(&self) -> Option<i64> {
        match self {
            Cancel::Replied | Cancel::Active => None,
            Cancel::TaskDone(id) | Cancel::EventDecided(id) => Some(*id),
        }
    }
}
```

In `spent`, change `AND e.origin = 'agent'` to `AND e.origin IN ('agent', 'idle')`. Update its doc comment's first line to:

```rust
/// Triggers the agent laid, or idleness laid, for that day out of its own
/// budget: a check inside a work session is the session's, not the day's, and
/// a dropped one gives its place back.
```

Add `pub origin: String,` to `Firing` after `wall_time`. In `read`, change the SELECT to:

```rust
        "SELECT id, prompt, cancel_if, cancel_ref, conversation_id, work_session_id,
                created_at, wall_time, origin
         FROM events WHERE id = ?1",
```

and add `origin: r.get(8)?,` to the struct literal.

In `cancelled`, add this arm before `_ => Ok(false),`:

```rust
        "active" => {
            let Some(since) =
                ev.created_at.as_deref().and_then(|t| t.parse::<jiff::Timestamp>().ok())
            else {
                return Ok(false);
            };
            Ok(crate::presence::last_active(conn, user_id)?
                .is_some_and(|at| at.as_second() >= since.as_second()))
        }
```

Change the doc comment on `cancelled` to:

```rust
/// Whether the reason this trigger was laid has already taken care of itself:
/// the user replied in the thread, finished or dropped the task, settled the
/// event it was waiting on, or came back after going quiet.
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --lib triggers:: runner::`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add server/src/triggers.rs
git commit -m "feat(server): idle triggers spend the budget and cancel on activity

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LZnmd9SLioNADcyRtSoFuK"
```

---

### Task 6: The idle check, the idle note and nudge stamps

**Files:**
- Create: `server/src/idle.rs`
- Modify: `server/src/lib.rs` (`pub mod idle;` between `pub mod harvest;` and `pub mod learn;`)
- Modify: `server/src/runner.rs` (`sweep_once`)

**Interfaces:**
- Consumes:
  - `presence::{stamp, last_active}` (Task 1)
  - `UserConfig::idle_nudge_min` (Task 4)
  - `triggers::{insert, allowance, spent, open_work_session, Cancel::Active}` (Task 5)
  - `calendar::quiet_window`
  - `plan::ensure`
  - table `notes` (Notes plan)
- Produces:
  - `pub const idle::ORIGIN: &str = "idle"`
  - `pub const idle::PROMPT: &str`
  - `pub fn idle::check(conn: &Connection, config_dir: &Path, now: jiff::Timestamp) -> anyhow::Result<Vec<i64>>`, which returns the event ids it laid
  - `pub fn idle::context(conn: &Connection, user_id: i64, now: jiff::Timestamp) -> rusqlite::Result<String>`
  - `pub fn idle::stamp_nudged(conn: &Connection, user_id: i64, ids: &[i64], now: jiff::Timestamp) -> rusqlite::Result<usize>`

- [ ] **Step 1: Write the failing tests**

Create `server/src/idle.rs` with only the tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-30 is a Wednesday.
    fn env(extra: &str) -> (Connection, tempfile::TempDir, i64) {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("defaults/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            p,
            format!("display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n{extra}"),
        )
        .unwrap();
        (conn, tmp, uid)
    }

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    fn seen(conn: &Connection, uid: i64, ts: &str) {
        conn.execute("UPDATE users SET last_active_at = ?1 WHERE id = ?2", (ts, uid)).unwrap();
    }

    fn note(conn: &Connection, uid: i64, text: &str, pinned: bool) -> i64 {
        conn.execute(
            "INSERT INTO notes (user_id, text, pinned, created_at)
             VALUES (?1, ?2, ?3, '2026-09-30T08:00:00Z')",
            (uid, text, pinned as i64),
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn noon() -> jiff::Timestamp {
        at("2026-09-30T12:00:00Z")
    }

    #[test]
    fn twenty_quiet_minutes_with_an_open_note_lay_one_idle_trigger() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:40:00Z");
        note(&conn, uid, "call the bank", false);

        let laid = check(&conn, tmp.path(), noon()).unwrap();
        assert_eq!(laid.len(), 1);
        let row: (String, String, Option<String>, String, String, Option<i64>) = conn
            .query_row(
                "SELECT e.kind, e.origin, e.cancel_if, e.wall_time, p.date, e.work_session_id
                 FROM events e JOIN plans p ON p.id = e.plan_id WHERE e.id = ?1",
                [laid[0]],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            ("trigger".into(), "idle".into(), Some("active".into()), "12:00".into(),
             "2026-09-30".into(), None)
        );
        assert!(check(&conn, tmp.path(), at("2026-09-30T12:00:30Z")).unwrap().is_empty(),
            "one pending at a time");
    }

    #[test]
    fn short_of_the_threshold_nothing_is_laid() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:41:00Z");
        note(&conn, uid, "call the bank", false);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
    }

    #[test]
    fn the_setting_moves_the_threshold_and_zero_turns_it_off() {
        let (conn, tmp, uid) = env("idle_nudge_min = 5\n");
        seen(&conn, uid, "2026-09-30T11:55:00Z");
        note(&conn, uid, "call the bank", false);
        assert_eq!(check(&conn, tmp.path(), noon()).unwrap().len(), 1);

        let (conn, tmp, uid) = env("idle_nudge_min = 0\n");
        seen(&conn, uid, "2026-09-30T08:00:00Z");
        note(&conn, uid, "call the bank", false);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
    }

    #[test]
    fn a_pinned_or_done_note_or_a_running_session_keeps_it_quiet() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        note(&conn, uid, "pinned", true);
        let done = note(&conn, uid, "done", false);
        conn.execute("UPDATE notes SET done_at = '2026-09-30T09:00:00Z' WHERE id = ?1", [done])
            .unwrap();
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());

        note(&conn, uid, "open", false);
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, planned_min, started_at)
             VALUES (?1, 'essay', 60, '2026-09-30T11:30:00Z')",
            [uid],
        )
        .unwrap();
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
        conn.execute("UPDATE work_sessions SET ended_at = '2026-09-30T11:40:00Z'", []).unwrap();
        assert_eq!(check(&conn, tmp.path(), noon()).unwrap().len(), 1);
    }

    #[test]
    fn a_quiet_window_or_the_close_of_the_day_holds_it_back() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        note(&conn, uid, "call the bank", false);
        crate::calendar::create(&conn, uid, crate::calendar::Fields {
            title: "school".into(), kind: "fixed".into(), quiet: Some(true),
            start_time: "11:30".into(), end_time: "12:30".into(),
            days: Some(crate::calendar::day_mask(&["wed"]).unwrap()), ..Default::default()
        })
        .unwrap();
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());

        let (conn, tmp, uid) = env("close_day_time = \"11:30\"\n");
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        note(&conn, uid, "call the bank", false);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());

        let (conn, tmp, uid) = env("close_day_time = \"\"\n");
        seen(&conn, uid, "2026-09-30T22:30:00Z");
        note(&conn, uid, "call the bank", false);
        assert_eq!(check(&conn, tmp.path(), at("2026-09-30T23:00:00Z")).unwrap().len(), 1,
            "a blank close of day is no cutoff");
    }

    #[test]
    fn a_spent_budget_lays_nothing() {
        let (conn, tmp, uid) = env("triggers_per_day = 1\n");
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        note(&conn, uid, "call the bank", false);
        let date: jiff::civil::Date = "2026-09-30".parse().unwrap();
        let plan_id = crate::plan::ensure(&conn, tmp.path(), "aki", uid, date).unwrap();
        crate::triggers::insert(&conn, plan_id, "15:00", "ask", "agent", None, None, None,
            at("2026-09-30T08:00:00Z")).unwrap();
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
    }

    #[test]
    fn the_idle_clock_restarts_at_the_last_idle_trigger() {
        let (conn, tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:40:00Z");
        note(&conn, uid, "call the bank", false);
        let first = check(&conn, tmp.path(), noon()).unwrap();
        conn.execute("UPDATE events SET status = 'done' WHERE id = ?1", [first[0]]).unwrap();

        assert!(check(&conn, tmp.path(), at("2026-09-30T12:01:00Z")).unwrap().is_empty());
        assert!(check(&conn, tmp.path(), at("2026-09-30T12:19:00Z")).unwrap().is_empty());
        assert_eq!(check(&conn, tmp.path(), at("2026-09-30T12:20:00Z")).unwrap().len(), 1);
    }

    #[test]
    fn a_user_not_seen_today_is_left_alone() {
        let (conn, tmp, uid) = env("");
        note(&conn, uid, "call the bank", false);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty(), "never seen");
        seen(&conn, uid, "2026-09-29T22:00:00Z");
        assert!(check(&conn, tmp.path(), at("2026-09-30T00:30:00Z")).unwrap().is_empty());
        assert!(check(&conn, tmp.path(), at("2026-09-30T09:00:00Z")).unwrap().is_empty());
    }

    #[test]
    fn an_account_without_checkins_is_left_alone() {
        let (conn, tmp, uid) = env("");
        crate::auth::set_category(&conn, "aki", "test").unwrap();
        seen(&conn, uid, "2026-09-30T11:00:00Z");
        note(&conn, uid, "call the bank", false);
        assert!(check(&conn, tmp.path(), noon()).unwrap().is_empty());
    }

    #[test]
    fn the_idle_note_lists_open_notes_with_ages_and_last_nudges() {
        let (conn, _tmp, uid) = env("");
        seen(&conn, uid, "2026-09-30T11:40:00Z");
        let bank = note(&conn, uid, "call the bank", false);
        conn.execute("UPDATE notes SET last_nudged_at = '2026-09-30T11:00:00Z' WHERE id = ?1", [bank])
            .unwrap();
        let milk = note(&conn, uid, "milk", false);
        conn.execute("UPDATE notes SET created_at = '2026-09-30T11:30:00Z' WHERE id = ?1", [milk])
            .unwrap();
        note(&conn, uid, "pinned thing", true);

        let text = context(&conn, uid, noon()).unwrap();
        assert!(text.contains("Nothing from the user for 20 min."), "{text}");
        assert!(text.contains(&format!("- {bank}: \"call the bank\", added 4 h ago, nudged 1 h ago")),
            "{text}");
        assert!(text.contains(&format!("- {milk}: \"milk\", added 30 min ago, never nudged")), "{text}");
        assert!(!text.contains("pinned thing"), "{text}");
    }

    #[test]
    fn a_nudge_stamps_only_the_users_own_open_notes() {
        let (conn, _tmp, uid) = env("");
        let other = crate::auth::create_user(&conn, "bo", "p", false).unwrap();
        let mine = note(&conn, uid, "mine", false);
        let done = note(&conn, uid, "done", false);
        conn.execute("UPDATE notes SET done_at = '2026-09-30T09:00:00Z' WHERE id = ?1", [done])
            .unwrap();
        let theirs = note(&conn, other, "theirs", false);

        assert_eq!(stamp_nudged(&conn, uid, &[mine, done, theirs, 404], noon()).unwrap(), 1);
        let stamped: Vec<(i64, Option<String>)> = {
            let mut stmt = conn.prepare("SELECT id, last_nudged_at FROM notes ORDER BY id").unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            stamped,
            vec![(mine, Some(noon().to_string())), (done, None), (theirs, None)]
        );
    }
}
```

Add `pub mod idle;` to `server/src/lib.rs`.

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --lib idle::`

Expected: fails to compile, because `check`, `context` and `stamp_nudged` are not found.

- [ ] **Step 3: Implement**

Put this at the top of `server/src/idle.rs`, above the tests:

```rust
use crate::triggers::{self, Cancel};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

pub const ORIGIN: &str = "idle";

pub const PROMPT: &str = "The user has gone quiet with notes still open. Decide whether one \
     of them is worth a nudge now.";

struct Seen {
    user_id: i64,
    username: String,
    category: String,
    last_active: String,
}

/// Lays an idle trigger for every user who has gone quiet with open notes and
/// returns the events laid. One user's failure is logged and skips only them.
pub fn check(conn: &Connection, config_dir: &Path, now: jiff::Timestamp) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id, username, category, last_active_at FROM users
         WHERE disabled = 0 AND last_active_at IS NOT NULL",
    )?;
    let seen: Vec<Seen> = stmt
        .query_map([], |r| {
            Ok(Seen {
                user_id: r.get(0)?,
                username: r.get(1)?,
                category: r.get(2)?,
                last_active: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut laid = Vec::new();
    for user in seen {
        match check_one(conn, config_dir, &user, now) {
            Ok(Some(id)) => laid.push(id),
            Ok(None) => {}
            Err(e) => {
                crate::log::record_throttled(
                    conn,
                    Some(user.user_id),
                    "runner_error",
                    &format!("idle check: {e:#}"),
                    now,
                    crate::log::ERROR_LOG_WINDOW_MINS,
                )?;
            }
        }
    }
    Ok(laid)
}

fn check_one(
    conn: &Connection,
    config_dir: &Path,
    user: &Seen,
    now: jiff::Timestamp,
) -> Result<Option<i64>> {
    let Ok(cfg) = crate::config::UserConfig::load(config_dir, &user.username) else {
        return Ok(None);
    };
    let threshold = i64::from(cfg.idle_nudge_min());
    if threshold == 0 || !cfg.features(&user.category).checkins {
        return Ok(None);
    }
    if triggers::open_work_session(conn, user.user_id)?.is_some() {
        return Ok(None);
    }
    let tz = jiff::tz::TimeZone::get(&cfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    let local = now.to_zoned(tz.clone());
    let last: jiff::Timestamp = user.last_active.parse()?;
    if last.to_zoned(tz.clone()).date() != local.date() {
        return Ok(None);
    }
    let since = match last_idle_laid(conn, user.user_id)? {
        Some(laid) if laid > last => laid,
        _ => last,
    };
    if now.as_second() - since.as_second() < threshold * 60 {
        return Ok(None);
    }
    if idle_pending(conn, user.user_id)? {
        return Ok(None);
    }
    if crate::calendar::quiet_window(conn, user.user_id, &tz, now)?.is_some() {
        return Ok(None);
    }
    let wall = format!("{:02}:{:02}", local.hour(), local.minute());
    let close = cfg.close_day_time();
    if !close.is_empty() && wall.as_str() >= close {
        return Ok(None);
    }
    if open_notes(conn, user.user_id)? == 0 {
        return Ok(None);
    }
    let date = local.date();
    let allowance = triggers::allowance(conn, config_dir, &user.username, user.user_id, date)?;
    if triggers::spent(conn, user.user_id, date)? >= allowance {
        return Ok(None);
    }
    let plan_id = crate::plan::ensure(conn, config_dir, &user.username, user.user_id, date)?;
    let id = triggers::insert(
        conn,
        plan_id,
        &wall,
        PROMPT,
        ORIGIN,
        Some(Cancel::Active),
        None,
        None,
        now,
    )?;
    crate::log::record(
        conn,
        Some(user.user_id),
        "trigger_laid",
        &format!("event {id} at {date} {wall}: idle"),
    )?;
    Ok(Some(id))
}

fn last_idle_laid(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<jiff::Timestamp>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT MAX(e.created_at) FROM events e JOIN plans p ON p.id = e.plan_id
             WHERE p.user_id = ?1 AND e.kind = ?2 AND e.origin = ?3",
            (user_id, triggers::KIND, ORIGIN),
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(raw.and_then(|s| s.parse().ok()))
}

fn idle_pending(conn: &Connection, user_id: i64) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE p.user_id = ?1 AND e.kind = ?2 AND e.origin = ?3
           AND e.status IN ('pending', 'snoozed', 'fired')",
        (user_id, triggers::KIND, ORIGIN),
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn open_notes(conn: &Connection, user_id: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM notes WHERE user_id = ?1 AND done_at IS NULL AND pinned = 0",
        [user_id],
        |r| r.get(0),
    )
}

fn ago(ts: &str, now: jiff::Timestamp) -> String {
    let Ok(t) = ts.parse::<jiff::Timestamp>() else {
        return ts.to_string();
    };
    let min = (now.as_second() - t.as_second()).max(0) / 60;
    match min {
        0..=59 => format!("{min} min"),
        60..=2879 => format!("{} h", min / 60),
        _ => format!("{} d", min / 1440),
    }
}

/// What an idle trigger's session reads under its prompt: how long the user
/// has been quiet, then each open unpinned note with its id, age and last nudge.
pub fn context(conn: &Connection, user_id: i64, now: jiff::Timestamp) -> rusqlite::Result<String> {
    let mut s = String::new();
    if let Some(at) = crate::presence::last_active(conn, user_id)? {
        s.push_str(&format!(
            "Nothing from the user for {} min.\n",
            (now.as_second() - at.as_second()).max(0) / 60
        ));
    }
    s.push_str("Open notes (id: text, age, last nudge):\n");
    let mut stmt = conn.prepare(
        "SELECT id, text, created_at, last_nudged_at FROM notes
         WHERE user_id = ?1 AND done_at IS NULL AND pinned = 0
         ORDER BY created_at, id LIMIT 20",
    )?;
    let rows = stmt.query_map([user_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (id, text, created, nudged) = row?;
        let nudge = match nudged {
            Some(t) => format!("nudged {} ago", ago(&t, now)),
            None => "never nudged".to_string(),
        };
        s.push_str(&format!("- {id}: {text:?}, added {} ago, {nudge}\n", ago(&created, now)));
    }
    Ok(s)
}

/// Marks the notes a nudge named; ids that are not this user's open notes are
/// ignored. Returns how many were stamped.
pub fn stamp_nudged(
    conn: &Connection,
    user_id: i64,
    ids: &[i64],
    now: jiff::Timestamp,
) -> rusqlite::Result<usize> {
    let mut n = 0;
    for id in ids {
        n += conn.execute(
            "UPDATE notes SET last_nudged_at = ?1
             WHERE id = ?2 AND user_id = ?3 AND done_at IS NULL",
            (now.to_string(), id, user_id),
        )?;
    }
    Ok(n)
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --lib idle::`

Expected: PASS (11 tests).

- [ ] **Step 5: Hook the check into the sweep**

In `server/src/runner.rs` `sweep_once`, add this directly after the `let note = |e: anyhow::Error| { … };` closure and before `let fired = fire_due(…)`:

```rust
        if let Err(e) = crate::idle::check(&conn, &state.config_dir, now) {
            note(e);
        }
```

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --lib runner:: && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --test triggers_api`

Expected: PASS. No existing test has a `last_active_at` together with open notes, so nothing new is laid.

- [ ] **Step 6: Commit**

```bash
git add server/src/idle.rs server/src/lib.rs server/src/runner.rs
git commit -m "feat(server): idle check lays an idle trigger for a quiet user with open notes

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LZnmd9SLioNADcyRtSoFuK"
```

---

### Task 7: Firing an idle trigger: context, `say` naming notes, and the prompt

**Files:**
- Modify: `server/src/tools/trigger_ops.rs` (`SayArgs`, `say`, a test)
- Modify: `server/src/triggers.rs` (`fire`)
- Modify: `config/defaults/prompts/trigger.md`
- Create: `server/tests/idle_api.rs`

**Interfaces:**
- Consumes:
  - `idle::{ORIGIN, context, stamp_nudged, check}` (Task 6)
  - `Firing.origin` (Task 5)
  - `presence::stamp` (Task 1)
  - `POST /api/presence` (Task 2)
- Produces: `say` accepts `{"text": string, "notes"?: [i64]}` and returns `{"said": text, "notes": [i64]}`.

- [ ] **Step 1: Write the failing tests**

Append this inside `mod tests` in `server/src/tools/trigger_ops.rs`:

```rust
    #[test]
    fn say_carries_the_notes_it_names() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "say",
            r#"{"text":"the bank closes at five","notes":[3]}"#).unwrap();
        assert_eq!(out["notes"], serde_json::json!([3]));
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Trigger, "say", r#"{"text":"hi"}"#)
            .unwrap();
        assert_eq!(out["notes"], serde_json::json!([]));
    }
```

Create `server/tests/idle_api.rs`:

```rust
mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use note_server::channels::mock::MockChannel;
use note_server::channels::Channel;
use note_server::providers::mock::MockLLM;
use note_server::providers::{ChatResponse, ToolCall};
use note_server::{api, auth, db, AppState};
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

struct World {
    app: axum::Router,
    cookie: String,
    state: AppState,
    push: Arc<MockChannel>,
    llm: Arc<MockLLM>,
    _cfg: TempDir,
}

/// A fixed-offset zone whose wall clock reads midday whenever the suite runs,
/// so half an hour ago is always today and before the close of the day.
fn midday_zone() -> String {
    let hour = i32::from(jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).hour());
    // Etc/GMT+N runs N hours behind UTC, so the sign reads backwards.
    format!("Etc/GMT{:+}", hour - 12)
}

async fn world(script: Vec<ChatResponse>) -> World {
    let cfg = common::config_dir();
    let dir = cfg.path().to_path_buf();
    std::fs::create_dir_all(dir.join("users/aki/templates")).unwrap();
    std::fs::write(dir.join("users/aki/templates/default.toml"), "events = []\n").unwrap();
    std::fs::write(dir.join("users/aki/user.toml"), format!("timezone = \"{}\"\n", midday_zone()))
        .unwrap();
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", true).unwrap();
    let push = Arc::new(MockChannel::new("mockpush"));
    let llm = Arc::new(MockLLM::scripted(script));
    let ladder: Vec<Arc<dyn Channel>> = vec![push.clone()];
    let state = AppState::new(conn, dir.clone(), dir)
        .with_channels(ladder)
        .with_providers(llm.clone(), None);
    let app = api::router(state.clone());
    let cookie = common::login(&app, "aki", "pw").await;
    World { app, cookie, state, push, llm, _cfg: cfg }
}

fn call(id: &str, name: &str, args: &str) -> ChatResponse {
    ChatResponse {
        text: String::new(),
        tool_calls: vec![ToolCall { id: id.into(), name: name.into(), args: args.into() }],
    }
}

fn rows<T: rusqlite::types::FromSql>(w: &World, sql: &str) -> Vec<T> {
    let conn = w.state.db.lock().unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    let out = stmt.query_map([], |r| r.get(0)).unwrap();
    out.collect::<rusqlite::Result<_>>().unwrap()
}

/// Quiet for half an hour with one open note; returns the note's id.
fn gone_quiet(w: &World) -> i64 {
    let conn = w.state.db();
    let then = jiff::Timestamp::now() - jiff::Span::new().minutes(30);
    conn.execute(
        "UPDATE users SET last_active_at = ?1 WHERE id = 1",
        [note_server::presence::stamp(then)],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO notes (user_id, text, pinned, created_at) VALUES (1, 'call the bank', 0, ?1)",
        [then.to_string()],
    )
    .unwrap();
    conn.last_insert_rowid()
}

#[tokio::test]
async fn a_quiet_user_with_an_open_note_is_nudged_about_it_once() {
    let w = world(vec![call("c1", "say", r#"{"text":"the bank closes at five","notes":[1]}"#)])
        .await;
    assert_eq!(gone_quiet(&w), 1);

    note_server::runner::sweep_once(&w.state);

    let seen = w.push.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1.body, "the bank closes at five");
    assert!(seen[0].1.actions.is_empty(), "a nudge carries no event buttons");
    let origins: Vec<String> = rows(&w, "SELECT origin FROM events WHERE kind = 'trigger'");
    assert_eq!(origins, vec!["idle"]);
    let nudged: Vec<Option<String>> = rows(&w, "SELECT last_nudged_at FROM notes WHERE id = 1");
    assert!(nudged[0].is_some(), "the named note carries the nudge");
    let opening = format!("{:?}", w.llm.seen()[0].messages.last().unwrap());
    assert!(opening.contains("call the bank"), "{opening}");
    assert!(opening.contains("Nothing from the user for 30 min."), "{opening}");

    note_server::runner::sweep_once(&w.state);
    assert_eq!(w.push.seen().len(), 1, "one quiet stretch, one nudge");
}

#[tokio::test]
async fn a_nudge_held_back_stays_held_until_another_stretch_of_quiet() {
    let w = world(vec![call("c1", "stay_quiet", r#"{"reason":"nothing on the list is urgent"}"#)])
        .await;
    gone_quiet(&w);

    note_server::runner::sweep_once(&w.state);
    note_server::runner::sweep_once(&w.state);

    assert!(w.push.seen().is_empty());
    let statuses: Vec<String> = rows(&w, "SELECT status FROM events WHERE kind = 'trigger'");
    assert_eq!(statuses, vec!["done"]);
    let nudged: Vec<Option<String>> = rows(&w, "SELECT last_nudged_at FROM notes WHERE id = 1");
    assert!(nudged[0].is_none());
}

#[tokio::test]
async fn coming_back_before_the_nudge_fires_calls_it_off() {
    let w = world(vec![]).await;
    gone_quiet(&w);
    {
        let conn = w.state.db();
        let laid =
            note_server::idle::check(&conn, &w.state.config_dir, jiff::Timestamp::now()).unwrap();
        assert_eq!(laid.len(), 1);
    }
    let res = w
        .app
        .clone()
        .oneshot(
            Request::post("/api/presence")
                .header(header::COOKIE, &w.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    note_server::runner::sweep_once(&w.state);

    assert!(w.llm.seen().is_empty(), "no session ran");
    assert!(w.push.seen().is_empty());
    let statuses: Vec<String> = rows(&w, "SELECT status FROM events WHERE kind = 'trigger'");
    assert_eq!(statuses, vec!["dropped"]);
    let why: Vec<String> =
        rows(&w, "SELECT detail FROM event_log WHERE kind = 'trigger_cancelled'");
    assert!(why[0].contains("active"), "{}", why[0]);
}
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --lib say_carries ; CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test --test idle_api`

Expected: `say_carries_the_notes_it_names` FAILS, because `notes` is an unknown field under `deny_unknown_fields`. `a_quiet_user_…` FAILS on the empty actions assertion, the `last_nudged_at` stamp and the missing idle note. `coming_back_…` PASSES already, since Task 5 did that part.

- [ ] **Step 3: Extend `say`**

In `server/src/tools/trigger_ops.rs`, change `SayArgs` to:

```rust
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SayArgs {
    /// One or two warm sentences, as the user will read them.
    pub text: String,
    /// The ids of the notes these words are about, when a nudge names any.
    #[serde(default)]
    pub notes: Vec<i64>,
}
```

and change the final line of `say` to:

```rust
    Ok(serde_json::json!({ "said": text, "notes": args.notes }))
```

- [ ] **Step 4: Teach `fire` about idle triggers**

In `server/src/triggers.rs` `fire`, replace:

```rust
        let opening = situation(&conn, fired.user_id, &ev, &tz);
```

with:

```rust
        let mut opening = situation(&conn, fired.user_id, &ev, &tz);
        if ev.origin == crate::idle::ORIGIN {
            opening.push_str(&crate::idle::context(&conn, fired.user_id, now).unwrap_or_default());
        }
```

In the `say` branch, replace:

```rust
        let _ = settle(&conn, ev.event_id, now);
        let _ = crate::log::record(
            &conn,
            Some(fired.user_id),
            "trigger_said",
```

with:

```rust
        let _ = settle(&conn, ev.event_id, now);
        if ev.origin == crate::idle::ORIGIN {
            let named: Vec<i64> =
                serde_json::from_value(result["notes"].clone()).unwrap_or_default();
            let _ = crate::idle::stamp_nudged(&conn, fired.user_id, &named, now);
        }
        let _ = crate::log::record(
            &conn,
            Some(fired.user_id),
            "trigger_said",
```

Replace the `actions:` expression with:

```rust
            actions: if ev.prompt == CLOSE_DAY_PROMPT {
                vec![crate::channels::Action {
                    label: "Carry to tomorrow".into(),
                    data: format!("carry:{}", fired.date),
                }]
            } else if ev.origin == crate::idle::ORIGIN {
                Vec::new()
            } else {
                crate::channels::event_actions(ev.event_id)
            },
```

- [ ] **Step 5: Add the idle section to the prompt**

Append this to `config/defaults/prompts/trigger.md`:

```markdown

## When the user has gone quiet

Some triggers are laid by the user's silence rather than by you. Their
opening lists the open notes, each with its id, how old it is and when you
last nudged about it, and how long since the user last did anything.

- Stay quiet when nothing on the list matters today, or when every note
  that does was nudged about in the last hour. Never name a note you
  nudged about less than an hour ago.
- Otherwise nudge about one note — the one that matters most right now —
  and put its id in `say`'s `notes`. One note, one or two sentences.
- Keep a first nudge light. Only a note already nudged about earlier today,
  with no word from the user since, may get a firmer line.
- Lay no follow-up for a quiet user: while they stay quiet, you will be
  asked again.
```

- [ ] **Step 6: Run the tests and confirm they pass**

Run: `cd /home/shuntia/Projects/note/server && CARGO_TARGET_DIR=/home/shuntia/Projects/note/target/idle-nudges cargo test`

Expected: PASS for the whole server suite, including `tool_fuzz` and `triggers_api`.

- [ ] **Step 7: Commit**

```bash
git add server/src/tools/trigger_ops.rs server/src/triggers.rs config/defaults/prompts/trigger.md server/tests/idle_api.rs
git commit -m "feat(server): idle triggers read the open notes and stamp the one they nudge about

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LZnmd9SLioNADcyRtSoFuK"
```

---

### Task 8: The web client pings presence

**Files:**
- Create: `web/src/presence.ts`, `web/src/presence.test.ts`
- Modify: `web/src/api.ts` (`api.presence`)
- Modify: `web/src/app.tsx` (import and effect)

**Interfaces:**
- Consumes: `POST /api/presence` (Task 2).
- Produces:
  - `startPresence(deps: PresenceDeps): () => void`
  - `PRESENCE_EVERY_MS = 60_000`
  - `api.presence(): Promise<void>`

- [ ] **Step 1: Write the failing test**

Create `web/src/presence.test.ts`:

```ts
import { afterEach, beforeEach, expect, test, vi } from 'vitest'
import { PRESENCE_EVERY_MS, startPresence } from './presence'

let target: EventTarget
let visible: boolean
let ping: ReturnType<typeof vi.fn>
let stop: () => void

beforeEach(() => {
  vi.useFakeTimers()
  target = new EventTarget()
  visible = true
  ping = vi.fn()
  stop = startPresence({ ping, target, visible: () => visible, now: () => Date.now() })
})

afterEach(() => {
  stop()
  vi.useRealTimers()
})

const touch = () => target.dispatchEvent(new Event('pointerdown'))

test('the first touch pings at once and the rest of the minute folds into one more', () => {
  touch()
  expect(ping).toHaveBeenCalledTimes(1)
  vi.advanceTimersByTime(10_000)
  touch()
  touch()
  expect(ping).toHaveBeenCalledTimes(1)
  vi.advanceTimersByTime(PRESENCE_EVERY_MS - 10_000)
  expect(ping).toHaveBeenCalledTimes(2)
})

test('an untouched page stays silent', () => {
  vi.advanceTimersByTime(PRESENCE_EVERY_MS * 5)
  expect(ping).not.toHaveBeenCalled()
})

test('a hidden page never pings', () => {
  visible = false
  touch()
  expect(ping).not.toHaveBeenCalled()
  visible = true
  touch()
  vi.advanceTimersByTime(1000)
  touch()
  visible = false
  vi.advanceTimersByTime(PRESENCE_EVERY_MS)
  expect(ping).toHaveBeenCalledTimes(1)
})

test('stopping drops the listeners and the waiting ping', () => {
  touch()
  vi.advanceTimersByTime(1000)
  touch()
  stop()
  vi.advanceTimersByTime(PRESENCE_EVERY_MS)
  touch()
  expect(ping).toHaveBeenCalledTimes(1)
})
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `pnpm -C /home/shuntia/Projects/note/web test`

Expected: FAIL, because `./presence` cannot be resolved.

- [ ] **Step 3: Implement**

Create `web/src/presence.ts`:

```ts
export const PRESENCE_EVERY_MS = 60_000

const INTERACTIONS = ['pointerdown', 'keydown', 'wheel', 'touchstart'] as const

export type PresenceDeps = {
  ping: () => void
  target: EventTarget
  visible: () => boolean
  now: () => number
}

// Pings at a touch of a visible page, then at most once a minute while touches keep coming.
export function startPresence({ ping, target, visible, now }: PresenceDeps): () => void {
  let last = -Infinity
  let timer: ReturnType<typeof setTimeout> | undefined
  const send = () => {
    last = now()
    ping()
  }
  const onTouch = () => {
    if (!visible() || timer !== undefined) return
    const wait = last + PRESENCE_EVERY_MS - now()
    if (wait <= 0) send()
    else
      timer = setTimeout(() => {
        timer = undefined
        if (visible()) send()
      }, wait)
  }
  for (const type of INTERACTIONS) target.addEventListener(type, onTouch, { passive: true })
  return () => {
    clearTimeout(timer)
    timer = undefined
    for (const type of INTERACTIONS) target.removeEventListener(type, onTouch)
  }
}
```

In `web/src/api.ts`, add this to the `api` object after `settings: …`:

```ts
  presence: () => request<void>('/api/presence', { method: 'POST' }),
```

In `web/src/app.tsx`, add `import { startPresence } from './presence'` among the local imports, in alphabetical position after `./prefs`. Add this effect directly before the `connectEvents` effect:

```tsx
  useEffect(() => {
    if (!me) return
    return startPresence({
      ping: () => void api.presence().catch(() => {}),
      target: window,
      visible: () => !document.hidden,
      now: Date.now,
    })
  }, [me])
```

- [ ] **Step 4: Run the checks and confirm they pass**

Run: `pnpm -C /home/shuntia/Projects/note/web build && pnpm -C /home/shuntia/Projects/note/web test`

Expected: the build succeeds, and all tests PASS, including the 4 new ones.

- [ ] **Step 5: Commit**

```bash
git add web/src/presence.ts web/src/presence.test.ts web/src/api.ts web/src/app.tsx
git commit -m "feat(web): ping presence while the page is visible and touched

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LZnmd9SLioNADcyRtSoFuK"
```

---

### Task 9: The idle nudge row in Settings

**Files:**
- Modify: `web/src/types.ts` (`EventOrigin`, `Settings`, `SettingsSaved`)
- Modify: `web/src/api.ts` (`WRITABLE_SETTINGS`)
- Modify: `web/src/views/Settings.tsx` (`EDITABLE`, a const, `draftOf`, the new row)

**Interfaces:**
- Consumes: `idle_nudge_min` in `GET` and `PUT /api/settings` (Task 4).
- Produces: `Settings.idle_nudge_min: number`, and `EventOrigin` includes `'idle'`.

- [ ] **Step 1: Types and the writable list**

In `web/src/types.ts`:

```ts
export type EventOrigin = 'template' | 'agent' | 'auto' | 'user' | 'idle'
```

In `Settings`, after `session_end_notify: boolean`:

```ts
  // Minutes quiet before Note may nudge about open notes; 0 is off.
  idle_nudge_min: number
```

In `SettingsSaved`'s `Pick` union, add `| 'idle_nudge_min'` after `| 'session_end_notify'`.

In `web/src/api.ts` `WRITABLE_SETTINGS`, add `'idle_nudge_min',` after `'session_end_notify',`.

- [ ] **Step 2: The Settings row**

In `web/src/views/Settings.tsx`:

Add `'idle_nudge_min',` to `EDITABLE` after `'pomodoro_break_min',`.

Below `const BREAK_MIN = …`:

```ts
const IDLE_MIN = { min: 0, max: 240 }
```

In `draftOf`, add this after `pomodoro_break_min: s.pomodoro_break_min,`:

```ts
    idle_nudge_min: s.idle_nudge_min,
```

Insert this row directly after the "Check-ins from Note" `FoldRow` block, before the `</Group>` that precedes `<Group head="You">`:

```tsx
        {loaded && (
          <FoldRow
            label="Nudge when idle"
            value={loaded.draft.idle_nudge_min ? `${loaded.draft.idle_nudge_min} min` : 'Off'}
            open={open === 'idle'}
            onToggle={fold('idle')}
          >
            {open === 'idle' && (
              <div className="set-fold-body">
                <input
                  aria-label="Minutes idle before a nudge"
                  type="number"
                  min={IDLE_MIN.min}
                  max={IDLE_MIN.max}
                  value={loaded.draft.idle_nudge_min}
                  onChange={(e) =>
                    edit(
                      'idle_nudge_min',
                      Math.min(IDLE_MIN.max, Math.max(IDLE_MIN.min, Math.round(Number(e.target.value) || 0))),
                    )
                  }
                  {...commitOn('idle')}
                />
                <Status save={save} row="idle" />
              </div>
            )}
          </FoldRow>
        )}
```

- [ ] **Step 3: Run the checks**

Run: `pnpm -C /home/shuntia/Projects/note/web build && pnpm -C /home/shuntia/Projects/note/web test`

Expected: the build succeeds and all tests PASS.

- [ ] **Step 4: See it**

Run the UI audit harness (server on 3299, vite on 5174; see memory "Note UI audit harness"). Open Settings, then "Nudge when idle". The row reads `20 min`. Set it to 0 and blur: the row reads `Off` and `✓ Saved` shows. Reload: it still reads `Off`.

- [ ] **Step 5: Commit**

```bash
git add web/src/types.ts web/src/api.ts web/src/views/Settings.tsx
git commit -m "feat(web): idle nudge setting

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LZnmd9SLioNADcyRtSoFuK"
```

---

## Self-Review

**Spec coverage (section 4):**

| Spec requirement | Where |
|---|---|
| `users.last_active_at`, at most once a minute | Task 1 |
| Stamped by API calls | Task 2 |
| Stamped by WebSocket, chat, Telegram, and the Matrix hook | Task 3 |
| `POST /api/presence` | Task 2 (server), Task 8 (client every 60 s while visible and interacted-with) |
| Per-minute check with all five conditions | Task 6 |
| Idle trigger with `origin = 'idle'`, counted against the budget, none when the budget is spent | Tasks 1, 5, 6 |
| Trigger kind with an idle note (open notes, ages, last nudges, minutes idle) | Tasks 6, 7 |
| `say`, `stay_quiet`, `wait_for` and `trigger_set` unchanged and available | Existing registry |
| `trigger.md` idle section | Task 7 |
| `last_nudged_at` stamped | Tasks 6, 7 |
| `cancel_if` `Active` checked at fire | Tasks 1, 5, 7 |
| `Replied` unchanged | Existing |
| `idle_nudge_min` in the API and Settings UI | Tasks 4, 9 |

**Type consistency:**
- `presence::{stamp, touch, last_active}` are used in Tasks 2, 3, 5, 6 and 7 with the same signatures.
- `idle::{ORIGIN, PROMPT, check, context, stamp_nudged}` are defined in Task 6 and used in Task 7.
- `Firing.origin` is defined in Task 5 and used in Task 7.
- `SayArgs.notes: Vec<i64>` appears in the tool result as `"notes"`.
- `idle_nudge_min` has the same spelling in config, API, types and Settings.
