# Horizon Server Groundwork Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the server what the Horizon UI needs: a time span on every event, two new per-user settings, a per-day silent toggle on an event, and "move to tomorrow".

**Architecture:** Routines keep `end_wall_time` NULL (that column is what makes a block a block, and a schema CHECK depends on it); they gain a `span_min` column and the API reports a computed end. Settings grow two fields through the existing `UserConfig` → `SettingsPatch` → `settings_body` chain. Two new event routes follow the shape of `/api/events/{id}/snooze`.

**Tech Stack:** Rust stable, axum 0.8, rusqlite, serde, jiff, toml. Tests: `cargo test` from the repo root (unit tests in-module, integration tests under `server/tests/` using `common::app_with_logged_in_user`).

**Spec:** `docs/superpowers/plans/2026-09-01-horizon-ui-spec.md` (section "Server needs").

## Global Constraints

- Every routine has a span; default span is 15 minutes when the template gives no `end_time`.
- Never red anywhere; not relevant server-side, but copy in error bodies stays plain and sentence case.
- Comments only at function declarations and only when the signature cannot say it (CLAUDE.md).
- Every commit: imperative summary line, `cargo test` green before it. Commit trailer:
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01UvaK6yJJPvZeJz6UYwJ9Rp`.
- Run all commands from the worktree root `/home/shuntia/Projects/note/.claude/worktrees/note-horizon`.

---

### Task 1: A span on every event

**Files:**
- Modify: `server/src/db.rs` (append migration v9 after the v8 string at :146-150; add a test beside `v8_adds_the_block_range_and_bell_and_keeps_blocks_silent`)
- Modify: `server/src/templates.rs` (`TemplateEvent` impl :39-61, `rows()` :185-203, `validate()` :258-260)
- Modify: `server/src/plan.rs` (`generate` :86-95, `events_for` :102-133, tests :384-390)
- Test: `server/tests/plan_api.rs`

**Interfaces:**
- Produces: `templates::wall_add(wall: &str, minutes: i64) -> String` (zero-padded HH:MM, clamped to 23:59); `TemplateEvent::span_min(&self) -> i64`; `PlanEvent.end_wall_time` is now `Some` for every event.

- [ ] **Step 1: Write the failing schema test** in `server/src/db.rs` tests module:

```rust
    #[test]
    fn v9_gives_every_event_a_span() {
        let conn = open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')", [])
            .unwrap();
        conn.execute("INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-09-01', 'x')", [])
            .unwrap();
        conn.execute("INSERT INTO events (plan_id, kind, wall_time) VALUES (1, 'nudge', '09:00')", [])
            .unwrap();
        let span: i64 = conn
            .query_row("SELECT span_min FROM events WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(span, 15);
        assert!(conn.execute("UPDATE events SET span_min = 0 WHERE id = 1", []).is_err());
    }
```

- [ ] **Step 2: Run it**: `cargo test -p note-server v9_gives` — expect FAIL (`no such column: span_min`).

- [ ] **Step 3: Add migration v9** to `MIGRATIONS` in `server/src/db.rs`, after the v8 entry:

```rust
    // v9
    "
    ALTER TABLE events ADD COLUMN span_min INTEGER NOT NULL DEFAULT 15
        CHECK (span_min > 0);
    ",
```

- [ ] **Step 4: Run** `cargo test -p note-server v9_gives` — expect PASS. Also run `cargo test -p note-server db::` to confirm the `newer_db_version_is_refused` test still passes (it sets version 99).

- [ ] **Step 5: Write the failing template tests** in `server/src/templates.rs` tests module:

```rust
    #[test]
    fn a_routine_may_carry_an_end_time_and_defaults_to_fifteen_minutes() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind='meds'\ntime='08:00'\ndays=['mon']\n\
             [[events]]\nkind='walk'\ntime='18:00'\nend_time='18:45'\ndays=['mon']\n");
        let t = Template::load(tmp.path(), "aki", "default").unwrap();
        assert_eq!(t.events[0].span_min(), 15);
        assert_eq!(t.events[1].span_min(), 45);
        let rows = t.rows();
        assert_eq!(rows[0].end_time.as_deref(), Some("08:15"));
        assert_eq!(rows[1].end_time.as_deref(), Some("18:45"));
        assert_eq!(rows[0].entry, Entry::Routine);
    }

```

  and, using the module's existing `bad(...)` helper (it loads a template string and returns the error text):

```rust
    #[test]
    fn a_routine_end_before_its_start_is_rejected() {
        assert!(bad("[[events]]\nkind='nudge'\ntime='09:00'\nend_time='08:30'\ndays=['mon']\n")
            .contains("end_time"));
        assert!(bad("[[events]]\nkind='nudge'\ntime='09:00'\nend_time='9:30'\ndays=['mon']\n")
            .contains("end_time"));
    }

    #[test]
    fn wall_add_pads_and_clamps() {
        assert_eq!(wall_add("09:00", 15), "09:15");
        assert_eq!(wall_add("23:50", 30), "23:59");
        assert_eq!(wall_add("08:05", 55), "09:00");
    }
```

  Also change the existing assertion at templates.rs:382-383 (`kind='nudge'\ntime='09:00'\nend_time='10:00'` must contain `end_time`) — a routine with a later end is now valid, so delete those two lines.

- [ ] **Step 6: Run** `cargo test -p note-server templates::` — expect the three new tests to FAIL (`span_min` / `wall_add` not found; end_time rejected).

- [ ] **Step 7: Implement** in `server/src/templates.rs`.

  Add after `valid_time`:

```rust
/// Adds `minutes` to a zero-padded wall time, never crossing midnight.
pub(crate) fn wall_add(wall: &str, minutes: i64) -> String {
    let (h, m) = wall.split_once(':').unwrap_or(("0", "0"));
    let total = (h.parse::<i64>().unwrap_or(0) * 60 + m.parse::<i64>().unwrap_or(0) + minutes)
        .clamp(0, 23 * 60 + 59);
    format!("{:02}:{:02}", total / 60, total % 60)
}

fn wall_minutes(wall: &str) -> i64 {
    let (h, m) = wall.split_once(':').unwrap_or(("0", "0"));
    h.parse::<i64>().unwrap_or(0) * 60 + m.parse::<i64>().unwrap_or(0)
}
```

  Add to `impl TemplateEvent`:

```rust
    /// How long a routine occupies, from its `end_time` or the fifteen-minute
    /// default; a block's range is stored on the event instead.
    pub fn span_min(&self) -> i64 {
        if self.is_block() {
            return 0;
        }
        match self.end_time.as_deref() {
            Some(end) => wall_minutes(end) - wall_minutes(&self.time),
            None => 15,
        }
    }

    /// The effective end of the entry, for both shapes.
    pub fn end(&self) -> String {
        match (self.is_block(), self.end_time.as_deref()) {
            (true, Some(end)) => end.to_string(),
            (true, None) => self.time.clone(),
            (false, _) => wall_add(&self.time, self.span_min()),
        }
    }
```

  In `rows()`, change `end_time: ev.end_time.clone(),` to `end_time: Some(ev.end()),`.

  In `validate()`, replace the trailing branch

```rust
            } else if let Some(end) = &ev.end_time {
                return Err(bad("end_time", end));
            }
```

  with

```rust
            } else if let Some(end) = ev.end_time.as_deref() {
                if !valid_time(end) || end <= ev.time.as_str() {
                    return Err(bad("end_time", end));
                }
            }
```

  `ScheduleRow.end_time` stays `Option<String>` (the client type already allows null); it is now always `Some`.

- [ ] **Step 8: Run** `cargo test -p note-server templates::` — expect PASS. Fix the existing test at :425 (`assert_eq!(rows[0].end_time, None)`) to `assert_eq!(rows[0].end_time.as_deref(), Some("08:15"))`.

- [ ] **Step 9: Write the failing plan tests.** In `server/src/plan.rs` tests, change the assertions at :384-386 to:

```rust
        assert_eq!(evs[0].entry, "routine");
        assert_eq!(evs[0].end_wall_time.as_deref(), Some("09:15"));
        assert!(!evs[0].alert);
```

  and add, next to `snooze_sets_status_and_pushes_time`:

```rust
    #[test]
    fn a_routine_end_follows_its_start_when_snoozed() {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')", [])
            .unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, 1, &tmpl(), date).unwrap();
        snooze(&conn, 1, 1, 20).unwrap().unwrap();
        let evs = events_for(&conn, 1, date).unwrap();
        assert_eq!(evs[0].wall_time, "09:20");
        assert_eq!(evs[0].end_wall_time.as_deref(), Some("09:35"));
    }
```

  (`tmpl()` is the existing fixture at the top of the tests module: a routine at 09:00 and a block 09:30–12:30 on the same weekday; `2026-08-31` is a Monday — check `tmpl()`'s `days` and use a date it matches. If the fixture lists `mon`, the date above is right.)

- [ ] **Step 10: Run** `cargo test -p note-server plan::` — expect the two to FAIL.

- [ ] **Step 11: Implement** in `server/src/plan.rs`.

  In `generate`, the INSERT becomes:

```rust
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, end_wall_time,
                                 flexibility, slide_window_min, channel, alert, span_min)
             VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            (
                plan_id, &ev.kind, &ev.time, &ev.end_time, ev.flexibility(),
                ev.slide_window_min(), &ev.channel, ev.alert(), ev.span_min().max(1),
            ),
        )?;
```

  In `events_for`, select `e.span_min` as the 14th column (append `, e.span_min` after `m.kind`) and build the row as:

```rust
        let end_wall_time: Option<String> = r.get(3)?;
        let wall_time: String = r.get(2)?;
        let span_min: i64 = r.get(13)?;
        let entry = if end_wall_time.is_some() { "block" } else { "routine" };
        Ok(PlanEvent {
            id: r.get(0)?,
            kind: r.get(1)?,
            end_wall_time: Some(
                end_wall_time.unwrap_or_else(|| crate::templates::wall_add(&wall_time, span_min)),
            ),
            wall_time,
            entry: entry.into(),
```

  (keep the remaining fields as they are). The `WHERE p.user_id = ?1 AND p.date = ?2` and the ordering do not change.

- [ ] **Step 12: Run** `cargo test -p note-server` — expect PASS across the crate. Grep for other callers that insert into `events` (`grep -n "INSERT INTO events" server/src`) — `schedule_ops.rs` (the agent's `schedule_insert`) inserts without `span_min`; the column default covers it, so no change is required, but confirm its tests still pass.

- [ ] **Step 13: Write the failing integration test** in `server/tests/plan_api.rs`:

```rust
#[tokio::test]
async fn every_event_reports_a_span() {
    let (app, cookie, cfg) = common::app_with_logged_in_user().await;
    let p = cfg.path().join("users/x/templates/default.toml");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        "[[events]]\nkind='checkin'\ntime='09:00'\ndays=['mon','tue','wed','thu','fri','sat','sun']\n\
         [[events]]\nkind='walk'\ntime='18:00'\nend_time='18:45'\ndays=['mon','tue','wed','thu','fri','sat','sun']\n",
    )
    .unwrap();
    let res = app
        .oneshot(
            Request::get("/api/plan/today?date=2026-09-01")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v[0]["entry"], "routine");
    assert_eq!(v[0]["end_wall_time"], "09:15");
    assert_eq!(v[1]["end_wall_time"], "18:45");
}
```

  Check `common::app_with_logged_in_user` for the username it creates (`settings_api.rs` asserts `display_name == "X"`; read `server/tests/common/mod.rs:49` for the username) and use it in the template path.

- [ ] **Step 14: Run** `cargo test -p note-server --test plan_api every_event` — expect PASS (the implementation is in place; if the user override path is wrong the first event will be `checkin_call` from the fixture template — fix the path, not the assertion).

- [ ] **Step 15: Commit**

```bash
git add server/src/db.rs server/src/templates.rs server/src/plan.rs server/tests/plan_api.rs
git commit -m "feat: every event has a span, routines default to fifteen minutes"
```

---

### Task 2: Two home settings

**Files:**
- Modify: `server/src/config.rs` (`UserConfig` :63-69 and its `save` doc comment)
- Modify: `server/src/api.rs` (`SettingsPatch` :489-496, `settings_body` :498-509, `settings_put` :558-610)
- Test: `server/tests/settings_api.rs`

**Interfaces:**
- Produces: `UserConfig.show_arc_between_sessions: bool` (default true), `UserConfig.counter: String` (`"remaining"` | `"elapsed"`, default `"remaining"`); both readable and writable on `/api/settings`.

- [ ] **Step 1: Write the failing tests** in `server/tests/settings_api.rs` (reuse the file's `get`/`put`/`json` helpers):

```rust
#[tokio::test]
async fn home_settings_default_and_round_trip() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let v = json(app.clone().oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["show_arc_between_sessions"], true);
    assert_eq!(v["counter"], "remaining");

    let res = app
        .clone()
        .oneshot(put(&cookie, r#"{"show_arc_between_sessions":false,"counter":"elapsed"}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json(res).await;
    assert_eq!(v["show_arc_between_sessions"], false);
    assert_eq!(v["counter"], "elapsed");

    let v = json(app.oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["show_arc_between_sessions"], false);
    assert_eq!(v["counter"], "elapsed");
}

#[tokio::test]
async fn counter_only_takes_the_two_words() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let res = app.oneshot(put(&cookie, r#"{"counter":"sideways"}"#)).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = json(res).await;
    assert!(v["error"].as_str().unwrap().starts_with("counter"));
}
```

- [ ] **Step 2: Run** `cargo test -p note-server --test settings_api home_settings counter_only` — expect FAIL (fields missing / 400 on unknown field).

- [ ] **Step 3: Implement.** In `server/src/config.rs`:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserConfig {
    pub display_name: String,
    pub timezone: String,
    pub template: String,
    #[serde(default = "default_nightly_time")]
    pub nightly_time: String,
    #[serde(default = "default_true")]
    pub show_arc_between_sessions: bool,
    #[serde(default = "default_counter")]
    pub counter: String,
}

fn default_true() -> bool {
    true
}

fn default_counter() -> String {
    "remaining".into()
}
```

  Change the `save` doc comment's "all four fields" to "every field". In `server/src/api.rs`:

```rust
const COUNTERS: [&str; 2] = ["remaining", "elapsed"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsPatch {
    display_name: Option<String>,
    timezone: Option<String>,
    nightly_time: Option<String>,
    template: Option<String>,
    show_arc_between_sessions: Option<bool>,
    counter: Option<String>,
    alerts: Option<Vec<AlertPatch>>,
}
```

  `settings_body` gains `"show_arc_between_sessions": cfg.show_arc_between_sessions, "counter": cfg.counter,`. In `settings_put`, after the `template` block:

```rust
    if let Some(show) = req.show_arc_between_sessions {
        cfg.show_arc_between_sessions = show;
    }
    if let Some(counter) = req.counter {
        if !COUNTERS.contains(&counter.as_str()) {
            return invalid_field("counter", "must be remaining or elapsed");
        }
        cfg.counter = counter;
    }
```

- [ ] **Step 4: Run** `cargo test -p note-server` — expect PASS. `config::UserConfig::load` deserialises old `user.toml` files thanks to the defaults; confirm with `cargo test -p note-server config::`.

- [ ] **Step 5: Commit**

```bash
git add server/src/config.rs server/src/api.rs server/tests/settings_api.rs
git commit -m "feat: settings carry the home arc and counter choices"
```

---

### Task 3: Silence one event for today

**Files:**
- Modify: `server/src/plan.rs` (add `set_alert` after `set_status` :317-326)
- Modify: `server/src/api.rs` (route table :37-40; handler after `event_drop` :907-913)
- Test: `server/tests/plan_api.rs`

**Interfaces:**
- Produces: `plan::set_alert(conn, user_id, event_id, alert) -> Result<Option<()>, AlertRefused>` with `pub enum AlertRefused { Block, Other(anyhow::Error) }`; `POST /api/events/{id}/alert` body `{"alert": bool}` → 200 | 404 | 409 (block).

- [ ] **Step 1: Write the failing integration test** in `server/tests/plan_api.rs`:

```rust
#[tokio::test]
async fn an_event_can_be_silenced_for_the_day() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let day = |app: axum::Router| async move {
        let res = app
            .oneshot(
                Request::get("/api/plan/today?date=2026-08-31")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()
    };
    let v = day(app.clone()).await;
    assert_eq!(v[0]["alert"], true);
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/events/1/alert")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"alert":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = day(app.clone()).await;
    assert_eq!(v[0]["alert"], false);
    let res = app
        .oneshot(
            Request::post("/api/events/999/alert")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"alert":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
```

  The closure captures `cookie` by move twice; if the borrow checker objects, clone the cookie into two locals before the closure (`let c1 = cookie.clone();`) and build two plain request blocks instead of the closure.

- [ ] **Step 2: Run** `cargo test -p note-server --test plan_api silenced` — expect FAIL (404 on the route).

- [ ] **Step 3: Implement.** In `server/src/plan.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum AlertRefused {
    #[error("a block never pings")]
    Block,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Turns the bell on or off for this day's instance only; the template keeps
/// its own setting for every other day.
pub fn set_alert(
    conn: &Connection,
    user_id: i64,
    event_id: i64,
    alert: bool,
) -> Result<Option<()>, AlertRefused> {
    let Some(ev) = owned_event(conn, user_id, event_id)? else {
        return Ok(None);
    };
    if ev.is_block {
        return Err(AlertRefused::Block);
    }
    conn.execute("UPDATE events SET alert = ?1 WHERE id = ?2", (alert, event_id))
        .map_err(anyhow::Error::from)?;
    Ok(Some(()))
}
```

  (`owned_event` is the existing private helper used by `snooze`; it returns `is_block`, `wall_time`, `status`. Its error type is `ShiftError`; if `?` cannot convert it into `AlertRefused`, map it: `.map_err(|e| AlertRefused::Other(anyhow::anyhow!(e.to_string())))?`.) Add a unit test in the plan tests module:

```rust
    #[test]
    fn a_block_cannot_take_a_bell() {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')", [])
            .unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, 1, &tmpl(), date).unwrap();
        assert!(matches!(set_alert(&conn, 1, 2, true), Err(AlertRefused::Block)));
        set_alert(&conn, 1, 1, false).unwrap().unwrap();
        assert!(!events_for(&conn, 1, date).unwrap()[0].alert);
    }
```

  In `server/src/api.rs`, add the route `.route("/api/events/{id}/alert", post(event_alert))` after the drop route, and the handler after `event_drop`:

```rust
#[derive(Deserialize)]
struct AlertReq {
    alert: bool,
}

async fn event_alert(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<AlertReq>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::plan::set_alert(&conn, user.id, id, req.alert) {
        Ok(Some(())) => StatusCode::OK.into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(crate::plan::AlertRefused::Block) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
```

- [ ] **Step 4: Run** `cargo test -p note-server` — expect PASS.

- [ ] **Step 5: Commit**

```bash
git add server/src/plan.rs server/src/api.rs server/tests/plan_api.rs
git commit -m "feat: silence a single event for the day"
```

---

### Task 4: Move an event to tomorrow

**Files:**
- Modify: `server/src/plan.rs` (add `move_to_tomorrow` after `set_alert`)
- Modify: `server/src/api.rs` (route; handler after `event_alert`)
- Test: `server/tests/plan_api.rs`

**Interfaces:**
- Produces: `plan::move_to_tomorrow(conn, user_id, event_id, template: &Template, tomorrow: Date) -> Result<Option<i64>, ShiftError>` (the new event's id); `POST /api/events/{id}/move_tomorrow` → 200 `{"event_id": n, "date": "YYYY-MM-DD"}` | 404 | 409 (decided, or a block).

- [ ] **Step 1: Write the failing integration test** in `server/tests/plan_api.rs`:

```rust
#[tokio::test]
async fn an_event_moves_to_tomorrow() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let fetch = |date: &'static str, cookie: String| {
        let app = app.clone();
        async move {
            let res = app
                .oneshot(
                    Request::get(format!("/api/plan/today?date={date}"))
                        .header(header::COOKIE, cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = res.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()
        }
    };
    let today = fetch("2026-08-31", cookie.clone()).await;
    let id = today[0]["id"].as_i64().unwrap();
    let res = app
        .clone()
        .oneshot(
            Request::post(format!("/api/events/{id}/move_tomorrow"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let moved: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(moved["date"], "2026-09-01");

    let today = fetch("2026-08-31", cookie.clone()).await;
    assert_eq!(today[0]["status"], "dropped");
    assert_eq!(today[0]["moved_to"]["date"], "2026-09-01");
    let tomorrow = fetch("2026-09-01", cookie.clone()).await;
    let copy = tomorrow
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == moved["event_id"])
        .expect("the copy lives in tomorrow's plan");
    assert_eq!(copy["wall_time"], today[0]["wall_time"]);
    assert_eq!(copy["status"], "pending");
}
```

  The test's "today" is a fixed date via the query parameter, so the handler must take the source event's own plan date, not the wall-clock date, to compute "tomorrow" — see Step 3.

- [ ] **Step 2: Run** `cargo test -p note-server --test plan_api moves_to_tomorrow` — expect FAIL (404).

- [ ] **Step 3: Implement.** In `server/src/plan.rs`:

```rust
/// Copies an undecided routine into the plan for the day after its own plan
/// date, creating that plan from `template` if needed, and marks today's
/// instance dropped with a pointer to the copy. A block is never moved.
pub fn move_to_tomorrow(
    conn: &Connection,
    user_id: i64,
    event_id: i64,
    template: &Template,
) -> Result<Option<(i64, jiff::civil::Date)>, ShiftError> {
    let Some(ev) = owned_event(conn, user_id, event_id)? else {
        return Ok(None);
    };
    if ev.is_block {
        return Ok(None);
    }
    if ev.status == "done" || ev.status == "dropped" {
        return Err(ShiftError::Decided { status: ev.status });
    }
    let date: String = conn
        .query_row(
            "SELECT p.date FROM events e JOIN plans p ON p.id = e.plan_id WHERE e.id = ?1",
            [event_id],
            |r| r.get(0),
        )
        .map_err(anyhow::Error::from)?;
    let tomorrow = date
        .parse::<jiff::civil::Date>()
        .map_err(anyhow::Error::from)?
        .tomorrow()
        .map_err(anyhow::Error::from)?;
    let tx = conn.unchecked_transaction().map_err(anyhow::Error::from)?;
    let plan_id = generate(conn, user_id, template, tomorrow)?;
    conn.execute(
        "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, end_wall_time,
                             flexibility, slide_window_min, channel, alert, span_min)
         SELECT ?1, kind, orig_wall_time, orig_wall_time, end_wall_time,
                flexibility, slide_window_min, channel, alert, span_min
         FROM events WHERE id = ?2",
        (plan_id, event_id),
    )
    .map_err(anyhow::Error::from)?;
    let new_id = conn.last_insert_rowid();
    conn.execute(
        "UPDATE events SET status = 'dropped', moved_to_event_id = ?1 WHERE id = ?2",
        (new_id, event_id),
    )
    .map_err(anyhow::Error::from)?;
    tx.commit().map_err(anyhow::Error::from)?;
    Ok(Some((new_id, tomorrow)))
}
```

  Notes for the implementer: `generate` opens its own transaction only when the connection is in autocommit, so calling it inside `unchecked_transaction` is the documented pattern (see the comment in `generate`). If `ShiftError` has no `From<anyhow::Error>`, wrap with `ShiftError::Other(...)` at each `map_err`. The copy uses `orig_wall_time` so a snoozed event lands at its planned time tomorrow.

  In `server/src/api.rs`, add `.route("/api/events/{id}/move_tomorrow", post(event_move_tomorrow))` and:

```rust
async fn event_move_tomorrow(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let ucfg = match crate::config::UserConfig::load(&state.config_dir, &user.username) {
        Ok(c) => c,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let tmpl = match crate::templates::Template::load(&state.config_dir, &user.username, &ucfg.template) {
        Ok(t) => t,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let conn = state.db.lock().unwrap();
    match crate::plan::move_to_tomorrow(&conn, user.id, id, &tmpl) {
        Ok(Some((event_id, date))) => {
            Json(serde_json::json!({ "event_id": event_id, "date": date.to_string() })).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(crate::plan::ShiftError::Decided { .. }) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
```

- [ ] **Step 4: Run** `cargo test -p note-server` — expect PASS. Also run `cargo test -p note-server --test tool_invariants --test full_day` explicitly; the nightly planner's handling of `moved_to_event_id` (`events_for` joins it) must be unchanged.

- [ ] **Step 5: Commit**

```bash
git add server/src/plan.rs server/src/api.rs server/tests/plan_api.rs
git commit -m "feat: move an event to tomorrow"
```

---

## Self-review notes

- Spec "Server needs" 1 → Task 1; 2 → Task 2; 3 is already supported (`snooze` takes minutes); 4 → Task 4; 5 (pause) is client-side, no task; 6 (memory feedback) is a chat message, no task. Per-event Silent (spec, Home → dots) → Task 3.
- `wall_add` is defined in Task 1 and used by Task 1's `events_for`; `set_alert`/`AlertRefused` (Task 3) and `move_to_tomorrow` (Task 4) are self-contained.
