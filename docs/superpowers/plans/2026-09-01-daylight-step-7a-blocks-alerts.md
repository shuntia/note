# Daylight Step 7 (server half) — Schedule blocks and per-routine alerts

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the day template two shapes of entry — point *routines* that may or may not ping, and time-ranged *blocks* that never ping and that the agent may reshape freely — and expose both through the plan and settings APIs so the Today and Settings views can render them.

**Architecture:** The template TOML gains three optional keys (`entry`, `end_time`, `alert`); `plan::generate` persists them onto each day's `events` row (new v8 columns `end_wall_time`, `alert`, plus `moved_to_event_id` for drop provenance). The runner's candidate query filters on `alert = 1`, which is the single choke point for push and WebSocket delivery alike, and a schema CHECK makes `alert = 1` impossible on a block. Bell toggles are written back into the per-user template override file, so they survive every nightly rebuild for free.

**Tech Stack:** Rust, axum, rusqlite (SQLite, `PRAGMA user_version` migrations), serde/toml, jiff, schemars (agent tool schemas).

**Spec:** `docs/superpowers/plans/2026-09-01-daylight-ui-spec.md` — step 7, items 7.1, 7.2, 7.5, plus the API surface 7.3/7.4 need. Mockups: `docs/superpowers/mockups/daylight/today-desktop-light.html` (Work time band, bell glyphs), `settings-desktop-light.html` (Schedule pane rows).

**Out of scope:** all React. A later agent renders Today's bands/bells and the Settings Schedule pane against the contract this plan produces.

## Global Constraints

- Additive and optional: a template with no `entry`, `end_time`, or `alert` keeps working and means routine / alert = true.
- Do not break the shipped shapes of `GET /api/plan/today` or `GET /api/settings`; only add fields.
- Copy rules on user-facing strings: sentence case, active voice, user vocabulary, no tool or system names.
- Accent = amber only; moss = done; clay = dropped/warn; never red. (No CSS in this plan; listed because it governs the strings the UI renders from these fields.)
- Comment policy: comment only what the code cannot say for itself; no process-history narration.
- Migration numbering: the next free step is v8.
- Baseline is 245 passing tests; `cargo test --workspace` must stay green throughout.

---

### Task 1: Template entries — routine or block, ping or silent

**Files:**
- Modify: `server/src/templates.rs`
- Test: `server/src/templates.rs` (`mod tests`)

**Interfaces:**
- Produces:
  - `pub enum Entry { Routine, Block }` — serde `rename_all = "snake_case"`, `Default` = `Routine`.
  - `TemplateEvent` fields: `kind: String`, `time: String`, `days: Vec<String>`, `entry: Entry`, `end_time: Option<String>`, `flexibility: Option<String>`, `slide_window_min: Option<i64>`, `channel: String`, `alert: Option<bool>`.
  - `TemplateEvent::is_block(&self) -> bool`
  - `TemplateEvent::flexibility(&self) -> &str` — `"slide"` for a block, else the declared value or `"fixed"`.
  - `TemplateEvent::slide_window_min(&self) -> i64` — `0` for a block, else the declared value or `0`.
  - `TemplateEvent::alert(&self) -> bool` — `false` for a block, else the declared value or `true`.
  - `#[derive(Default)]` on `TemplateEvent` so existing construction sites can use `..Default::default()`.
- Consumes: nothing.

Callers currently read `ev.flexibility` / `ev.slide_window_min` as plain fields (`plan.rs`, `outreach_ops.rs` construct-only). After this task those two become methods; update `plan::generate` in the same task to call them so the crate compiles.

- [ ] **Step 1: Write the failing tests**

Add to `server/src/templates.rs` tests:

```rust
#[test]
fn a_block_carries_a_range_never_pings_and_is_always_reshapable() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "defaults/templates/default.toml",
        "[[events]]\nentry='block'\nkind='Work time'\ntime='09:30'\nend_time='12:30'\ndays=['mon']\n");
    let t = Template::load(tmp.path(), "aki", "default").unwrap();
    let ev = &t.events[0];
    assert!(ev.is_block());
    assert_eq!(ev.end_time.as_deref(), Some("12:30"));
    assert_eq!(ev.flexibility(), "slide");
    assert_eq!(ev.slide_window_min(), 0);
    assert!(!ev.alert());
}

#[test]
fn a_routine_defaults_to_pinging_and_keeps_its_flexibility() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "defaults/templates/default.toml",
        "[[events]]\nkind='nudge'\ntime='09:00'\ndays=['mon']\nflexibility='slide'\nslide_window_min=30\n");
    let ev = &Template::load(tmp.path(), "aki", "default").unwrap().events[0];
    assert!(!ev.is_block());
    assert!(ev.alert());
    assert_eq!(ev.flexibility(), "slide");
    assert_eq!(ev.slide_window_min(), 30);

    write(tmp.path(), "defaults/templates/default.toml",
        "[[events]]\nkind='nudge'\ntime='09:00'\ndays=['mon']\nalert=false\n");
    let ev = &Template::load(tmp.path(), "aki", "default").unwrap().events[0];
    assert!(!ev.alert());
    assert_eq!(ev.flexibility(), "fixed");
}

#[test]
fn malformed_entries_fail_to_load() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = |toml: &str| -> String {
        write(tmp.path(), "defaults/templates/default.toml", toml);
        Template::load(tmp.path(), "aki", "default").unwrap_err().to_string()
    };
    assert!(bad("[[events]]\nentry='block'\nkind='Work'\ntime='09:30'\ndays=['mon']\n")
        .contains("end_time"));
    assert!(bad("[[events]]\nentry='block'\nkind='Work'\ntime='09:30'\nend_time='9:30'\ndays=['mon']\n")
        .contains("end_time"));
    assert!(bad("[[events]]\nentry='block'\nkind='Work'\ntime='12:30'\nend_time='09:30'\ndays=['mon']\n")
        .contains("end_time"));
    assert!(bad("[[events]]\nentry='block'\nkind='Work'\ntime='09:30'\nend_time='12:30'\ndays=['mon']\nalert=true\n")
        .contains("alert"));
    assert!(bad("[[events]]\nentry='block'\nkind='Work'\ntime='09:30'\nend_time='12:30'\ndays=['mon']\nflexibility='fixed'\n")
        .contains("flexibility"));
    assert!(bad("[[events]]\nkind='nudge'\ntime='09:00'\nend_time='10:00'\ndays=['mon']\n")
        .contains("end_time"));
    assert!(bad("[[events]]\nentry='band'\nkind='Work'\ntime='09:30'\ndays=['mon']\n")
        .contains("entry"));
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p note-server templates::`
Expected: compile error — no field `entry`, no method `is_block`.

- [ ] **Step 3: Implement**

```rust
#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Entry {
    #[default]
    Routine,
    Block,
}

#[derive(Debug, Default, Deserialize)]
pub struct TemplateEvent {
    pub kind: String,
    pub time: String,
    pub days: Vec<String>,
    #[serde(default)]
    pub entry: Entry,
    #[serde(default)]
    pub end_time: Option<String>,
    #[serde(default)]
    pub flexibility: Option<String>,
    #[serde(default)]
    pub slide_window_min: Option<i64>,
    #[serde(default = "default_channel")]
    pub channel: String,
    #[serde(default)]
    pub alert: Option<bool>,
}

impl TemplateEvent {
    pub fn is_block(&self) -> bool { self.entry == Entry::Block }

    /// A block is always the agent's to reshape, so it carries no declared
    /// flexibility of its own.
    pub fn flexibility(&self) -> &str {
        if self.is_block() { "slide" } else { self.flexibility.as_deref().unwrap_or("fixed") }
    }

    pub fn slide_window_min(&self) -> i64 {
        if self.is_block() { 0 } else { self.slide_window_min.unwrap_or(0) }
    }

    pub fn alert(&self) -> bool {
        !self.is_block() && self.alert.unwrap_or(true)
    }
}
```

`Default` for `channel` is `""`, which `validate` rejects; the `default_channel` serde default still applies on load.

Extend `validate` with, per event:

```rust
if self.is_block() {
    let Some(end) = ev.end_time.as_deref() else {
        return Err(bad("end_time", "(missing; a block needs a start and an end)"));
    };
    if !valid_time(end) || end <= ev.time.as_str() {
        return Err(bad("end_time", end));
    }
    if ev.flexibility.is_some() {
        return Err(bad("flexibility", "(a block is always reshapable)"));
    }
    if ev.slide_window_min.is_some() {
        return Err(bad("slide_window_min", "(a block is always reshapable)"));
    }
    if ev.alert.is_some() {
        return Err(bad("alert", "(a block never pings)"));
    }
} else if let Some(end) = &ev.end_time {
    return Err(bad("end_time", end));
}
```

Keep the existing `valid_time`, day, and empty-`kind` checks; replace the direct
`flexibility`/`slide_window_min` reads with the accessors. A bad `entry` value is
rejected by serde before `validate` runs — the deserialize error already names
`entry`, so `bad("entry", …)` is not needed.

`Template::load`'s error already prefixes the file path; leave it.

- [ ] **Step 4: Update `plan::generate` to compile**

In `server/src/plan.rs`, the insert becomes:

```rust
(plan_id, &ev.kind, &ev.time, ev.flexibility(), ev.slide_window_min(), &ev.channel),
```

and every `TemplateEvent { … }` literal in the crate's tests gains `..Default::default()` with
`flexibility: Some("slide".into())`, `slide_window_min: Some(60)` etc. Sites:
`server/src/plan.rs`, `server/src/runner.rs`, `server/src/context.rs`,
`server/src/tools/schedule_ops.rs`, `server/tests/tool_invariants.rs`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --workspace`
Expected: PASS, count above 245.

- [ ] **Step 6: Commit**

```bash
git add server/ docs/superpowers/plans/2026-09-01-daylight-step-7a-blocks-alerts.md
git commit -m "feat: template entries are routines or blocks, routines carry a bell"
```

---

### Task 2: Persist the shape onto the day's events (migration v8)

**Files:**
- Modify: `server/src/db.rs` (migration v8), `server/src/plan.rs`
- Test: `server/src/db.rs`, `server/src/plan.rs` (`mod tests`)

**Interfaces:**
- Produces:
  - Columns `events.end_wall_time TEXT` (NULL = routine), `events.alert INTEGER NOT NULL DEFAULT 1`, `events.moved_to_event_id INTEGER REFERENCES events(id)`.
  - `PlanEvent` fields, in this JSON order: `id, kind, wall_time, end_wall_time, entry, status, flexibility, slide_window_min, channel, alert, moved_to`.
  - `pub struct MovedTo { pub event_id: i64, pub date: String, pub wall_time: String, pub kind: String }` serialized under `moved_to`, omitted when absent.
- Consumes: Task 1's `TemplateEvent` accessors.

- [ ] **Step 1: Write the failing tests**

`server/src/db.rs`:

```rust
#[test]
fn v8_adds_the_block_range_and_bell_and_keeps_blocks_silent() {
    let conn = open_memory().unwrap();
    conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')", []).unwrap();
    conn.execute("INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-08-31', 'x')", []).unwrap();
    conn.execute("INSERT INTO events (plan_id, kind, wall_time) VALUES (1, 'nudge', '09:00')", []).unwrap();
    let (end, alert): (Option<String>, i64) = conn
        .query_row("SELECT end_wall_time, alert FROM events WHERE id = 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(end, None);
    assert_eq!(alert, 1);
    assert!(conn.execute("UPDATE events SET alert = 2 WHERE id = 1", []).is_err());
    assert!(conn
        .execute("UPDATE events SET end_wall_time = '12:30' WHERE id = 1", [])
        .is_err(), "a block must not keep its bell");
    conn.execute("UPDATE events SET alert = 0, end_wall_time = '12:30' WHERE id = 1", []).unwrap();
}
```

`server/src/plan.rs`:

```rust
#[test]
fn generate_stores_the_block_range_and_bell_state() {
    let conn = crate::db::open_memory().unwrap();
    let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
    let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
    let t = Template { events: vec![
        TemplateEvent { kind: "Work time".into(), time: "09:30".into(), days: vec!["mon".into()],
            entry: crate::templates::Entry::Block, end_time: Some("12:30".into()),
            channel: "push".into(), ..Default::default() },
        TemplateEvent { kind: "meds".into(), time: "08:00".into(), days: vec!["mon".into()],
            alert: Some(false), channel: "push".into(), ..Default::default() },
    ]};
    generate(&conn, uid, &t, date).unwrap();
    let evs = events_for(&conn, uid, date).unwrap();
    assert_eq!(evs[0].kind, "meds");
    assert_eq!(evs[0].entry, "routine");
    assert_eq!(evs[0].end_wall_time, None);
    assert!(!evs[0].alert);
    assert_eq!(evs[1].kind, "Work time");
    assert_eq!(evs[1].entry, "block");
    assert_eq!(evs[1].end_wall_time.as_deref(), Some("12:30"));
    assert!(!evs[1].alert);
    assert_eq!(evs[1].flexibility, "slide");
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p note-server db:: plan::`
Expected: FAIL — no such column `end_wall_time`; no field `entry` on `PlanEvent`.

- [ ] **Step 3: Implement the migration**

Append to `MIGRATIONS` in `server/src/db.rs`:

```rust
    // v8
    "
    ALTER TABLE events ADD COLUMN end_wall_time TEXT;
    ALTER TABLE events ADD COLUMN alert INTEGER NOT NULL DEFAULT 1
        CHECK (alert IN (0, 1) AND (alert = 0 OR end_wall_time IS NULL));
    ALTER TABLE events ADD COLUMN moved_to_event_id INTEGER REFERENCES events(id);
    ",
```

- [ ] **Step 4: Implement the model**

`server/src/plan.rs`:

```rust
#[derive(Debug, Serialize)]
pub struct MovedTo {
    pub event_id: i64,
    pub date: String,
    pub wall_time: String,
    pub kind: String,
}

#[derive(Debug, Serialize)]
pub struct PlanEvent {
    pub id: i64,
    pub kind: String,
    pub wall_time: String,
    pub end_wall_time: Option<String>,
    pub entry: String,
    pub status: String,
    pub flexibility: String,
    pub slide_window_min: i64,
    pub channel: String,
    pub alert: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved_to: Option<MovedTo>,
}
```

`generate` inserts the two new columns:

```rust
conn.execute(
    "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, end_wall_time,
                         flexibility, slide_window_min, channel, alert)
     VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8)",
    (plan_id, &ev.kind, &ev.time, &ev.end_time, ev.flexibility(),
     ev.slide_window_min(), &ev.channel, ev.alert()),
)?;
```

`events_for` left-joins the move target:

```rust
let mut stmt = conn.prepare(
    "SELECT e.id, e.kind, e.wall_time, e.end_wall_time, e.status, e.flexibility,
            e.slide_window_min, e.channel, e.alert,
            m.id, mp.date, m.wall_time, m.kind
     FROM events e JOIN plans p ON p.id = e.plan_id
     LEFT JOIN events m ON m.id = e.moved_to_event_id
     LEFT JOIN plans mp ON mp.id = m.plan_id
     WHERE p.user_id = ?1 AND p.date = ?2 ORDER BY e.wall_time",
)?;
```

mapping `entry` as `if end_wall_time.is_some() { "block" } else { "routine" }` and
`moved_to` from the four optional columns (all present together, since the join
is on a foreign key).

- [ ] **Step 5: Run the tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add server/
git commit -m "feat: events carry a block range, a bell, and a move target"
```

---

### Task 3: Silent routines and blocks never reach a channel

**Files:**
- Modify: `server/src/runner.rs:83-89` (the candidate query)
- Test: `server/src/runner.rs` (`mod tests`), `server/tests/delivery_day.rs`

**Interfaces:**
- Consumes: `events.alert` from Task 2.
- Produces: nothing new; `fire_due` simply never yields a silent routine or a block.

- [ ] **Step 1: Write the failing tests**

`server/src/runner.rs`:

```rust
#[test]
fn a_silent_routine_never_fires_but_stays_on_the_plan() {
    let (conn, tmp, uid) = setup("UTC");
    let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
    let mut t = one_event_template("09:00");
    t.events[0].alert = Some(false);
    crate::plan::generate(&conn, uid, &t, date).unwrap();

    let now: jiff::Timestamp = "2026-08-31T12:00:00Z".parse().unwrap();
    assert!(fire_due(&conn, tmp.path(), now).unwrap().is_empty());

    let evs = crate::plan::events_for(&conn, uid, date).unwrap();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].status, "pending");
    crate::plan::set_status(&conn, uid, evs[0].id, "done").unwrap();
    assert_eq!(crate::plan::events_for(&conn, uid, date).unwrap()[0].status, "done");
}

#[test]
fn a_block_is_never_a_delivery_candidate() {
    let (conn, tmp, uid) = setup("UTC");
    let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
    let t = Template { events: vec![TemplateEvent {
        kind: "Work time".into(), time: "09:30".into(),
        days: vec!["mon".into(),"tue".into(),"wed".into(),"thu".into(),"fri".into(),"sat".into(),"sun".into()],
        entry: crate::templates::Entry::Block, end_time: Some("12:30".into()),
        channel: "push".into(), ..Default::default()
    }]};
    crate::plan::generate(&conn, uid, &t, date).unwrap();
    let now: jiff::Timestamp = "2026-08-31T23:00:00Z".parse().unwrap();
    assert!(fire_due(&conn, tmp.path(), now).unwrap().is_empty());
    assert_eq!(crate::plan::events_for(&conn, uid, date).unwrap()[0].status, "pending");
}
```

`server/tests/delivery_day.rs` — extend the existing simulated day: add to the
template a `Work time` block 09:30–12:30 and a silent `stretch` routine at 10:30,
then assert the 10:01 sweep still fires exactly the one `nudge`, and that a sweep
at 13:00 JST fires nothing at all.

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p note-server runner::`
Expected: FAIL — `fire_due` returns 1 candidate where 0 is expected.

- [ ] **Step 3: Implement**

In `fire_due`, the candidate query's WHERE clause becomes:

```sql
WHERE e.status IN ('pending','snoozed') AND e.alert = 1
```

with the comment above `fire_due` extended by one sentence: a block carries
`alert = 0` by schema, so the same filter covers both blocks and silent routines.

- [ ] **Step 4: Run the tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add server/
git commit -m "feat: silent routines and blocks fire no delivery"
```

---

### Task 4: Settings API reads and writes the bells

**Files:**
- Modify: `server/src/templates.rs` (row view + override writer), `server/src/api.rs:479-574`
- Test: `server/src/templates.rs`, `server/tests/settings_api.rs`

**Interfaces:**
- Produces:
  - `pub struct ScheduleRow { pub index: usize, pub kind: String, pub entry: Entry, pub time: String, pub end_time: Option<String>, pub days: Vec<String>, pub flexibility: String, pub slide_window_min: i64, pub channel: String, pub alert: bool }` (`Serialize`).
  - `Template::rows(&self) -> Vec<ScheduleRow>`
  - `pub fn set_alerts(config_dir: &Path, user: &str, name: &str, changes: &[(usize, bool)]) -> Result<(), AlertError>` writing `users/<user>/templates/<name>.toml`.
  - `pub enum AlertError { OutOfRange(usize), Block(usize), Io(anyhow::Error) }`
  - `GET /api/settings` gains `"schedule": [ScheduleRow…]`.
  - `PUT /api/settings` gains optional `"alerts": [{"index": u, "alert": bool}]`, and its response body gains `"schedule"`.
- Consumes: Task 1's template model.

- [ ] **Step 1: Write the failing tests**

`server/src/templates.rs`:

```rust
#[test]
fn toggling_a_bell_writes_a_user_override_and_leaves_the_default_alone() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "defaults/templates/default.toml", concat!(
        "[[events]]\nkind='meds'\ntime='08:00'\ndays=['mon']\n",
        "[[events]]\nentry='block'\nkind='Work time'\ntime='09:30'\nend_time='12:30'\ndays=['mon']\n"));
    set_alerts(tmp.path(), "aki", "default", &[(0, false)]).unwrap();
    let t = Template::load(tmp.path(), "aki", "default").unwrap();
    assert!(!t.events[0].alert());
    assert!(t.events[1].is_block());
    let default = std::fs::read_to_string(tmp.path().join("defaults/templates/default.toml")).unwrap();
    assert!(!default.contains("alert"), "the shared default was rewritten: {default}");

    set_alerts(tmp.path(), "aki", "default", &[(0, true)]).unwrap();
    assert!(Template::load(tmp.path(), "aki", "default").unwrap().events[0].alert());

    assert!(matches!(set_alerts(tmp.path(), "aki", "default", &[(1, false)]), Err(AlertError::Block(1))));
    assert!(matches!(set_alerts(tmp.path(), "aki", "default", &[(9, false)]), Err(AlertError::OutOfRange(9))));
}
```

`server/tests/settings_api.rs`:

```rust
#[tokio::test]
async fn get_lists_one_row_per_template_entry() {
    let (app, cookie, cfg) = common::app_with_logged_in_user().await;
    write(cfg.path(), "defaults/templates/default.toml", concat!(
        "[[events]]\nkind='meds'\ntime='08:00'\ndays=['mon']\n",
        "[[events]]\nentry='block'\nkind='Work time'\ntime='09:30'\nend_time='12:30'\ndays=['mon']\n"));
    let v = json(app.oneshot(get(&cookie)).await.unwrap()).await;
    let rows = v["schedule"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["index"], 0);
    assert_eq!(rows[0]["kind"], "meds");
    assert_eq!(rows[0]["entry"], "routine");
    assert_eq!(rows[0]["alert"], true);
    assert_eq!(rows[0]["end_time"], serde_json::Value::Null);
    assert_eq!(rows[1]["entry"], "block");
    assert_eq!(rows[1]["time"], "09:30");
    assert_eq!(rows[1]["end_time"], "12:30");
    assert_eq!(rows[1]["alert"], false);
}

#[tokio::test]
async fn a_toggled_bell_persists_and_survives_a_nightly_rebuild() {
    let (app, cookie, state, cfg) = common::app_with_logged_in_user_and_state().await;
    let res = app.clone().oneshot(put(&cookie, r#"{"alerts":[{"index":0,"alert":false}]}"#)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(json(res).await["schedule"][0]["alert"], false);
    assert!(cfg.path().join("users/aki/templates/default.toml").exists());

    let v = json(app.oneshot(get(&cookie)).await.unwrap()).await;
    assert_eq!(v["schedule"][0]["alert"], false);

    // the nightly job rebuilds tomorrow from the same template file
    let ucfg = note_server::config::UserConfig::load(cfg.path(), "aki").unwrap();
    let tmpl = note_server::templates::Template::load(cfg.path(), "aki", &ucfg.template).unwrap();
    let conn = state.db.lock().unwrap();
    let date: jiff::civil::Date = "2026-09-02".parse().unwrap();
    note_server::plan::generate(&conn, 1, &tmpl, date).unwrap();
    let evs = note_server::plan::events_for(&conn, 1, date).unwrap();
    assert!(!evs[0].alert);
    assert!(note_server::runner::fire_due(&conn, cfg.path(), "2026-09-02T23:00:00Z".parse().unwrap())
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_bell_on_a_block_or_a_missing_row_is_rejected() {
    let (app, cookie, cfg) = common::app_with_logged_in_user().await;
    write(cfg.path(), "defaults/templates/default.toml",
        "[[events]]\nentry='block'\nkind='Work time'\ntime='09:30'\nend_time='12:30'\ndays=['mon']\n");
    for body in [r#"{"alerts":[{"index":0,"alert":false}]}"#, r#"{"alerts":[{"index":4,"alert":false}]}"#] {
        let res = app.clone().oneshot(put(&cookie, body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "accepted {body}");
        assert!(json(res).await["error"].as_str().unwrap().contains("alerts"));
    }
    assert!(!cfg.path().join("users/aki/templates/default.toml").exists());
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test --workspace settings`
Expected: FAIL — `set_alerts` not found; `schedule` is null.

- [ ] **Step 3: Implement `set_alerts` and the row view**

In `server/src/templates.rs`:

```rust
#[derive(Debug, Serialize)]
pub struct ScheduleRow {
    pub index: usize,
    pub kind: String,
    pub entry: Entry,
    pub time: String,
    pub end_time: Option<String>,
    pub days: Vec<String>,
    pub flexibility: String,
    pub slide_window_min: i64,
    pub channel: String,
    pub alert: bool,
}

impl Template {
    pub fn rows(&self) -> Vec<ScheduleRow> { /* enumerate, using the accessors */ }
}

#[derive(Debug, thiserror::Error)]
pub enum AlertError {
    #[error("no entry at index {0}")]
    OutOfRange(usize),
    #[error("entry {0} is a block, and blocks never ping")]
    Block(usize),
    #[error(transparent)]
    Io(#[from] anyhow::Error),
}
```

`set_alerts` loads the effective template file as a `toml::Value` (user override
if present, else the default), validates each index against the parsed
`Template` (bounds, and that the entry is not a block), sets `alert` on the
matching `[[events]]` table, then writes the whole document to the user's own
override path through `crate::context::write_atomic`, creating the directory.
Rejecting before any write keeps a bad request from leaving a half-written
override behind.

- [ ] **Step 4: Implement the API surface**

`server/src/api.rs`:

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlertPatch {
    index: usize,
    alert: bool,
}
```

added to `SettingsPatch` as `alerts: Option<Vec<AlertPatch>>`.

`settings_body` gains a `schedule` argument:

```rust
fn settings_body(cfg: &crate::config::UserConfig, schedule: Vec<crate::templates::ScheduleRow>) -> serde_json::Value
```

Both `settings_get` and `settings_put` compute it with a helper that loads the
effective template and degrades to an empty list, so a template that no longer
parses still lets the user reach Settings and pick another one:

```rust
fn schedule_rows(state: &AppState, user: &str, template: &str) -> Vec<crate::templates::ScheduleRow> {
    crate::templates::Template::load(&state.config_dir, user, template)
        .map(|t| t.rows())
        .unwrap_or_default()
}
```

In `settings_put`, the alerts are applied after the four scalar fields are
merged and validated, against `cfg.template` as it stands after the merge, and
before `cfg.save`:

```rust
if let Some(alerts) = req.alerts {
    let changes: Vec<(usize, bool)> = alerts.iter().map(|a| (a.index, a.alert)).collect();
    if let Err(e) = crate::templates::set_alerts(&state.config_dir, &user.username, &cfg.template, &changes) {
        return invalid_field("alerts", &e.to_string());
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add server/
git commit -m "feat: settings reads and toggles the per-entry bells"
```

---

### Task 5: The agent reshapes blocks and can never touch a bell

**Files:**
- Modify: `server/src/plan.rs` (a `reshape` operation), `server/src/tools/schedule_ops.rs`, `server/src/tools/mod.rs`
- Test: `server/src/plan.rs`, `server/src/tools/schedule_ops.rs`

**Interfaces:**
- Produces:
  - `plan::reshape(conn, user_id, event_id, start: Option<&str>, end: Option<&str>) -> Result<Option<()>, ShiftError>` — `Ok(None)` when the event is not the user's or is not a block.
  - Tool `schedule_reshape` with args `{ event_id: i64, start?: "HH:MM", end?: "HH:MM" }`, registered in all three session kinds.
  - `plan::shift` and `plan::snooze` return `Ok(None)` for a block: a block carries an end as well as a start, so moving it through either would leave a range whose end precedes its start.
- Consumes: Task 2's `end_wall_time`.

- [ ] **Step 1: Write the failing tests**

`server/src/tools/schedule_ops.rs` tests (extend `env()` with a block at 09:30–12:30 as event 3):

```rust
#[test]
fn the_agent_reshapes_a_block_but_not_a_routine() {
    let (conn, tmp) = env();
    dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "schedule_reshape",
        r#"{"event_id":3,"start":"10:00","end":"13:00"}"#).unwrap();
    let (start, end): (String, String) = conn
        .query_row("SELECT wall_time, end_wall_time FROM events WHERE id=3", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((start.as_str(), end.as_str()), ("10:00", "13:00"));

    // moving only the end is a reshape too
    dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "schedule_reshape",
        r#"{"event_id":3,"end":"14:00"}"#).unwrap();

    let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "schedule_reshape",
        r#"{"event_id":1,"start":"10:00","end":"11:00"}"#).unwrap_err();
    assert_eq!(e.kind, "not_found", "a routine has no shape to change");

    for (raw, why) in [
        (r#"{"event_id":3}"#, "nothing to change"),
        (r#"{"event_id":3,"start":"9:00"}"#, "unpadded start"),
        (r#"{"event_id":3,"start":"15:00","end":"14:00"}"#, "end before start"),
    ] {
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "schedule_reshape", raw).unwrap_err();
        assert_eq!(e.kind, "rejected", "{why}");
    }
}

#[test]
fn no_schedule_tool_accepts_an_alert_flag() {
    let (conn, tmp) = env();
    for (tool, raw) in [
        ("schedule_reshape", r#"{"event_id":3,"start":"10:00","alert":false}"#),
        ("schedule_slide", r#"{"event_id":1,"minutes":5,"alert":false}"#),
        ("schedule_drop", r#"{"event_id":2,"alert":false}"#),
        ("schedule_insert", r#"{"date":"2026-08-31","kind":"nudge","time":"16:00","flexibility":"drop","channel":"push","alert":false}"#),
    ] {
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, tool, raw).unwrap_err();
        assert_eq!(e.kind, "invalid_args", "{tool} accepted an alert flag");
    }
    let bells: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT alert FROM events ORDER BY id").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    };
    assert_eq!(bells, vec![1, 1, 0]);
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p note-server schedule_ops::`
Expected: FAIL — `unknown_tool: schedule_reshape`.

- [ ] **Step 3: Implement `plan::reshape`**

```rust
/// Moves and resizes a block. A block carries no slide window — the agent owns
/// its shape — but a `done` or `dropped` block is a settled user decision and is
/// refused, exactly as in `shift`.
pub fn reshape(
    conn: &Connection,
    user_id: i64,
    event_id: i64,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<Option<()>, ShiftError>
```

It reads `wall_time`, `end_wall_time`, `status` for the owned event, returns
`Ok(None)` when the row is missing or `end_wall_time IS NULL`, errors
`Decided` on done/dropped, and otherwise updates `wall_time` and
`end_wall_time` (leaving `orig_wall_time` alone — a block's origin is still
where the template put it). Ordering (`end > start`) and `HH:MM` shape are the
tool's job, since they are argument validation.

- [ ] **Step 4: Implement the tool**

```rust
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReshapeArgs {
    pub event_id: i64,
    /// New start, zero-padded HH:MM.
    pub start: Option<String>,
    /// New end, zero-padded HH:MM.
    pub end: Option<String>,
}
```

`reshape` validates: at least one of start/end; each supplied value passes
`crate::templates::valid_time`; the resulting range has `end > start` (reading
the unchanged side from the row). Register `"schedule_reshape"` in `CHECKIN`,
`TALK`, and `NIGHTLY`, and describe it as:

```rust
"schedule_reshape" => (
    "Move or resize a block of time on the day's plan. Blocks are the only \
     entries with a start and an end; they never notify the user.",
    schema::<schedule_ops::ReshapeArgs>(),
),
```

The alert flag is enforced by absence: no tool's args struct has an `alert`
field and every one of them is `deny_unknown_fields`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --workspace`
Expected: PASS. `session_surfaces_are_nested_subsets` and
`schemas_cover_the_registry_and_are_objects` must stay green.

- [ ] **Step 6: Commit**

```bash
git add server/
git commit -m "feat: the agent reshapes blocks and never touches a bell"
```

---

### Task 6: Where a dropped event went

**Files:**
- Modify: `server/src/plan.rs`, `server/src/tools/schedule_ops.rs`
- Test: `server/src/tools/schedule_ops.rs`, `server/tests/plan_api.rs`

**Interfaces:**
- Produces: `schedule_drop` gains optional `moved_to_event_id: Option<i64>`; `plan::set_moved_to(conn, user_id, event_id, target_id) -> Result<bool>`; `PlanEvent.moved_to` (Task 2) is populated.
- Consumes: Task 2's `moved_to_event_id` column.

Spec item 2.3 wants Today to render `dropped — <where it went>` when the agent
rescheduled the event. `schedule_drop` and `schedule_insert` are two independent
calls with no link between them, so the server cannot infer the destination.
The narrowest truthful model is an explicit link the agent states at drop time
and the server verifies: the id of an event the caller owns.

- [ ] **Step 1: Write the failing test**

`server/src/tools/schedule_ops.rs`:

```rust
#[test]
fn a_drop_can_name_the_event_it_moved_to() {
    let (conn, tmp) = env();
    let out = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "schedule_insert",
        r#"{"date":"2026-08-31","kind":"nudge","time":"17:00","flexibility":"drop","channel":"push"}"#).unwrap();
    let moved = out["event_id"].as_i64().unwrap();
    dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "schedule_drop",
        &format!(r#"{{"event_id":2,"moved_to_event_id":{moved}}}"#)).unwrap();

    let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
    let evs = crate::plan::events_for(&conn, 1, date).unwrap();
    let dropped = evs.iter().find(|e| e.id == 2).unwrap();
    assert_eq!(dropped.status, "dropped");
    let to = dropped.moved_to.as_ref().unwrap();
    assert_eq!((to.event_id, to.wall_time.as_str(), to.kind.as_str()), (moved, "17:00", "nudge"));
    assert_eq!(to.date, "2026-08-31");

    // a plain drop stays a plain drop
    let e = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "schedule_drop",
        r#"{"event_id":2,"moved_to_event_id":9999}"#).unwrap_err();
    assert_eq!(e.kind, "rejected");
}

#[test]
fn a_drop_cannot_point_at_itself_or_someone_elses_event() {
    let (conn, tmp) = env();
    let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_drop",
        r#"{"event_id":2,"moved_to_event_id":2}"#).unwrap_err();
    assert_eq!(e.kind, "rejected");
    let status: String =
        conn.query_row("SELECT status FROM events WHERE id=2", [], |r| r.get(0)).unwrap();
    assert_eq!(status, "pending", "a rejected drop must change nothing");
}
```

`server/tests/plan_api.rs`: after the existing user drop through
`POST /api/events/1/drop`, assert `v[0]["moved_to"]` is absent — a user's own
drop carries no destination.

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p note-server schedule_ops::`
Expected: FAIL — unknown field `moved_to_event_id`.

- [ ] **Step 3: Implement**

`DropArgs` gains:

```rust
    /// The event this one moved to, when the drop is a reschedule. Insert the
    /// replacement first, then name its id here.
    #[serde(default)]
    pub moved_to_event_id: Option<i64>,
```

`drop_event` resolves the target through `plan::event_gate` (owner-scoped)
before setting the status: an unknown, unowned, or self-referential target is
`rejected` and nothing is written. `plan::set_moved_to` writes the column under
the same ownership predicate the other plan writes use.

- [ ] **Step 4: Run the tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add server/
git commit -m "feat: a rescheduled drop records where the event went"
```

---

### Task 7: The agent's plan view shows blocks and silent routines

**Files:**
- Modify: `server/src/context.rs:88-95`
- Test: `server/src/context.rs` (`mod tests`)

**Interfaces:**
- Consumes: `PlanEvent.end_wall_time`, `PlanEvent.alert`.
- Produces: nothing programmatic; the routine line keeps its exact current shape,
  which `server/tests/full_day.rs` asserts on.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_plan_section_distinguishes_blocks_and_silent_routines() {
    let tmp = cfg_dir();
    let conn = crate::db::open_memory().unwrap();
    let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
    let tmpl = crate::templates::Template { events: vec![
        crate::templates::TemplateEvent {
            kind: "Work time".into(), time: "09:30".into(), days: vec!["mon".into()],
            entry: crate::templates::Entry::Block, end_time: Some("12:30".into()),
            channel: "push".into(), ..Default::default() },
        crate::templates::TemplateEvent {
            kind: "meds".into(), time: "08:00".into(), days: vec!["mon".into()],
            alert: Some(false), channel: "push".into(), ..Default::default() },
    ]};
    let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
    crate::plan::generate(&conn, uid, &tmpl, date).unwrap();
    let now: jiff::Timestamp = "2026-08-31T12:00:00Z".parse().unwrap();
    let out = assemble(&conn, tmp.path(), uid, "aki", now).unwrap();
    assert!(out.contains("- 09:30-12:30 Work time [block]"), "{out}");
    assert!(out.contains("- 08:00 meds [pending] via push (silent)"), "{out}");
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p note-server context::`
Expected: FAIL — the block renders as a point event.

- [ ] **Step 3: Implement**

```rust
for e in &events {
    match &e.end_wall_time {
        Some(end) => out.push_str(&format!("- {}-{} {} [block]\n", e.wall_time, end, e.kind)),
        None => out.push_str(&format!(
            "- {} {} [{}] via {}{}\n",
            e.wall_time, e.kind, e.status, e.channel,
            if e.alert { "" } else { " (silent)" },
        )),
    }
}
```

- [ ] **Step 4: Run the whole suite**

Run: `cargo test --workspace`
Expected: PASS, well above 245.

- [ ] **Step 5: Commit**

```bash
git add server/
git commit -m "feat: the agent's day view names blocks and silent routines"
```

---

## The contract the UI agent codes against

`GET /api/plan/today` — array of:

```json
{
  "id": 12,
  "kind": "Work time",
  "wall_time": "09:30",
  "end_wall_time": "12:30",
  "entry": "block",
  "status": "pending",
  "flexibility": "slide",
  "slide_window_min": 0,
  "channel": "push",
  "alert": false,
  "moved_to": { "event_id": 19, "date": "2026-08-31", "wall_time": "17:00", "kind": "Wind-down walk" }
}
```

`entry` is `"routine"` or `"block"`; `end_wall_time` is null on routines;
`moved_to` is present only on an event the agent dropped while naming its
replacement (render `dropped — moved to <wall_time>`; a plain drop renders
`dropped`).

`GET /api/settings` — the four existing fields, `templates`, `timezones`, plus:

```json
"schedule": [
  { "index": 0, "kind": "meds", "entry": "routine", "time": "08:00", "end_time": null,
    "days": ["mon"], "flexibility": "fixed", "slide_window_min": 0, "channel": "push", "alert": true }
]
```

`PUT /api/settings` — the existing optional scalars plus
`"alerts": [{"index": 0, "alert": false}]`; the response is the four scalars and
`schedule`. A block index or an out-of-range index is a 400 naming `alerts`.

Agent tools: `schedule_reshape { event_id, start?, end? }`;
`schedule_drop { event_id, moved_to_event_id? }`. No tool accepts `alert`.

`POST /api/events/{id}/shift` and `/snooze` answer 404 for a block — Today must
render no ±15 chips and no Later on a band. `/done` and `/drop` still work on
one, and on a silent routine.

## Self-review

- Spec 7.1 → Tasks 1 and 2. 7.2 → Task 3. 7.5 → Task 5. The 7.3/7.4 API need → Tasks 2 and 4. Spec 2.3's missing field → Task 6. Task 7 keeps the agent's own view honest about both new shapes.
- The four acceptance boxes map to: `a_silent_routine_never_fires_but_stays_on_the_plan`, `a_block_is_never_a_delivery_candidate`, `a_toggled_bell_persists_and_survives_a_nightly_rebuild`, `the_agent_reshapes_a_block_but_not_a_routine` + `no_schedule_tool_accepts_an_alert_flag`.
- Names used across tasks: `Entry`, `TemplateEvent::alert()`, `ScheduleRow`, `set_alerts`, `AlertError`, `plan::reshape`, `plan::set_moved_to`, `MovedTo` — each defined once, in the task that introduces it.
