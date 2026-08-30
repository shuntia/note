# Note Server Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Retire the deferred debt ledgered across the foundation, providers, and channels phases: status-code consistency, nightly date semantics, error-log throttling, template validation gaps, placeholder copy, and talk/admin API error paths.

**Architecture:** Six independent, small server-side changes. No new modules, no schema changes, no new dependencies. Each task tightens an existing seam and pins it with tests.

**Tech Stack:** Rust (axum, rusqlite, jiff, tokio), existing offline test harness under `server/tests/`.

**Spec:** docs/superpowers/specs/2026-08-29-note-design.md

## Global Constraints

- No `MutexGuard` may be held across network I/O, provider calls, or channel `deliver()` calls.
- `channels::deliver_event`, `runner::sweep_once`, and the nightly sweep never return errors and never panic on per-user failures.
- All tests run offline; no real network anywhere.
- Tool-layer failures are typed rejections (`rejected` / `not_found` / `invalid_args` / `internal`), never partial state. API status mapping: 404 unknown-or-unowned, 400 malformed input, 409 conflict with a settled user decision, 429 throttled, 502 upstream (provider) failure.
- Wall times are zero-padded `HH:MM` strings; they are compared lexicographically once stored.
- Comment policy (CLAUDE.md): comments only at declarations and only where the signature cannot speak; no process-history narration anywhere, including test names.

---

### Task 1: Snooze rejects settled events the way shift does

Snoozing a `done`/`dropped` event currently reports `not_found` (tool) / 404 (API), while shift reports `rejected` / 409. Make snooze use the same `ShiftError::Decided` path: a settled decision is a conflict, not a missing event.

**Files:**
- Modify: `server/src/plan.rs` (fn `snooze`, its doc comment, and its tests)
- Modify: `server/src/tools/schedule_ops.rs` (fn `snooze`)
- Modify: `server/src/api.rs` (fn `event_snooze`)
- Test: `server/tests/plan_api.rs`

**Interfaces:**
- Produces: `plan::snooze(conn, user_id, event_id, minutes) -> Result<Option<()>, ShiftError>` (was `anyhow::Result<Option<()>>`). `Err(ShiftError::Decided { status })` for `done`/`dropped`; `Ok(None)` stays "not the user's event".

- [ ] **Step 1: Change `plan::snooze` to the `ShiftError` return type**

In `server/src/plan.rs`, replace the whole `snooze` function with:

```rust
/// Postpones delivery: any owned, undecided event (pending/snoozed/fired) can
/// be snoozed regardless of flexibility, and the slide window does not apply —
/// snooze is "not now", not a schedule change. A `done` or `dropped` event is a
/// settled user decision and is refused, exactly as in `shift`.
pub fn snooze(conn: &Connection, user_id: i64, event_id: i64, minutes: i64) -> Result<Option<()>, ShiftError> {
    if !(1..=24 * 60).contains(&minutes) {
        return Err(ShiftError::Other(anyhow::anyhow!(
            "snooze minutes must be in 1..=1440, got {minutes}"
        )));
    }
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT e.wall_time, e.status FROM events e
             JOIN plans p ON p.id = e.plan_id
             WHERE e.id = ?1 AND p.user_id = ?2",
            (event_id, user_id),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((wall, status)) = row else { return Ok(None) };
    if status == "done" || status == "dropped" {
        return Err(ShiftError::Decided { status });
    }
    let total = (parse_minutes(&wall)? + minutes).clamp(0, 23 * 60 + 59);
    conn.execute(
        "UPDATE events SET wall_time = ?1, status = 'snoozed' WHERE id = ?2",
        (format!("{:02}:{:02}", total / 60, total % 60), event_id),
    )?;
    Ok(Some(()))
}
```

Note `parse_minutes` returns `anyhow::Result`; `?` converts through `ShiftError::Other` via `#[from]`.

- [ ] **Step 2: Update the plan.rs unit test**

In `snooze_sets_status_and_pushes_time`, replace the two lines after `// decided events cannot be snoozed` with:

```rust
        conn.execute("UPDATE events SET status='done' WHERE id=?1", [ev_id]).unwrap();
        let err = snooze(&conn, uid, ev_id, 10).unwrap_err();
        assert!(matches!(err, ShiftError::Decided { .. }), "got {err:?}");
```

The later `assert!(snooze(&conn, uid, ev_id, 0).is_err());` and the ownership `is_none()` assertion still compile unchanged.

- [ ] **Step 3: Map the tool and the API route**

In `server/src/tools/schedule_ops.rs`, replace the `snooze` match with:

```rust
    match crate::plan::snooze(conn, ctx.user_id, args.event_id, args.minutes) {
        Ok(Some(())) => Ok(serde_json::json!({ "ok": true })),
        Ok(None) => Err(ToolError::not_found(format!(
            "no snoozable event {} for this user", args.event_id
        ))),
        Err(e @ crate::plan::ShiftError::Decided { .. }) => Err(ToolError::rejected(e.to_string())),
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
```

In `server/src/api.rs`, replace the `event_snooze` match with:

```rust
    match crate::plan::snooze(&conn, user.id, id, req.minutes) {
        Ok(Some(())) => StatusCode::OK.into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(crate::plan::ShiftError::Decided { .. }) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
```

- [ ] **Step 4: Add the tool-level and API-level regression tests**

In `server/src/tools/schedule_ops.rs` tests, add:

```rust
    #[test]
    fn snooze_on_a_decided_event_is_rejected_not_missing() {
        let (conn, tmp) = env();
        let ev_id = seeded_event(&conn);
        conn.execute("UPDATE events SET status='done' WHERE id=?1", [ev_id]).unwrap();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_snooze",
            &format!(r#"{{"event_id":{ev_id},"minutes":10}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");
    }
```

(Reuse the file's existing env/seed helpers; if the seed helper has a different name, adapt the call, not the assertion.)

In `server/tests/plan_api.rs`, add:

```rust
#[tokio::test]
async fn snooze_on_a_decided_event_is_conflict() {
    let (app, cookie, state, _cfg) = common::app_with_logged_in_user_and_state().await;
    let res = req(&app, &cookie, "GET", "/api/plan/today", None).await;
    assert_eq!(res.status(), StatusCode::OK);
    let evs: serde_json::Value = body_json(res).await;
    let id = evs[0]["id"].as_i64().unwrap();
    {
        let conn = state.db.lock().unwrap();
        conn.execute("UPDATE events SET status='done' WHERE id=?1", [id]).unwrap();
    }
    let res = req(&app, &cookie, "POST", &format!("/api/events/{id}/snooze"),
        Some(r#"{"minutes":15}"#)).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}
```

(Match the request/body helper names already used in `plan_api.rs`; the shift-409 test added in the channels phase is the pattern to copy.)

- [ ] **Step 5: Run `cargo test`, expect all green, commit**

```bash
git add server/src/plan.rs server/src/tools/schedule_ops.rs server/src/api.rs server/tests/plan_api.rs
git commit -m "fix: snoozing a settled event is a conflict, matching shift"
```

---

### Task 2: Evening nightly runs plan the next day

`nightly.rs` plans the *current* local date. The spec says the nightly run builds "tomorrow's plan while the user sleeps" — equivalent only for early-morning `nightly_time`. Rule: a `nightly_time` before 12:00 keeps planning the current local date (the run happens after midnight, so that date *is* the sleeper's "tomorrow"); a `nightly_time` at or after 12:00 runs before the user sleeps and must plan the next local date.

**Files:**
- Modify: `server/src/nightly.rs`

**Interfaces:**
- Produces: `fn plan_date(local: &jiff::Zoned, nightly_time: &str) -> jiff::civil::Date` (private to nightly.rs). Both `run_for_user` and `due` must compute the date through it — they share the debrief-row idempotency check, so they must agree.

- [ ] **Step 1: Add the helper and use it in both places**

In `server/src/nightly.rs`, replace `local_date` with:

```rust
/// The date this nightly run plans and debriefs. An early-morning
/// `nightly_time` runs after midnight, so the current local date is the
/// sleeper's coming day; from noon onward the run precedes sleep and targets
/// the next date. Falls back to the current date if `tomorrow` overflows.
fn plan_date(local: &jiff::Zoned, nightly_time: &str) -> jiff::civil::Date {
    let evening = nightly_time
        .split(':')
        .next()
        .and_then(|h| h.parse::<u8>().ok())
        .is_some_and(|h| h >= 12);
    if evening {
        local.date().tomorrow().unwrap_or_else(|_| local.date())
    } else {
        local.date()
    }
}
```

In `run_for_user`, replace the `let date = local_date(&ucfg, now);` line with:

```rust
    let tz = jiff::tz::TimeZone::get(&ucfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    let date = plan_date(&now.to_zoned(tz), &ucfg.nightly_time);
```

In `due`, replace the debrief-existence query's date argument: the `has` query currently uses `local.date().to_string()`; change it to use the shared helper:

```rust
        let target = plan_date(&local, &ucfg.nightly_time);
        let has: i64 = conn.query_row(
            "SELECT COUNT(*) FROM debriefs WHERE user_id = ?1 AND date = ?2",
            (id, target.to_string()),
            |r| r.get(0),
        )?;
```

- [ ] **Step 2: Add the tests**

In `server/src/nightly.rs` tests, add:

```rust
    #[test]
    fn evening_nightly_plans_the_next_local_date() {
        let (db, tmp) = env("Asia/Tokyo", "22:00");
        let llm = MockLLM::scripted(vec![
            ChatResponse { text: "tomorrow looks calm".into(), tool_calls: vec![] },
        ]);
        // 2026-08-31T14:00Z = 23:00 JST on 2026-08-31 — past a 22:00 nightly_time
        let now: jiff::Timestamp = "2026-08-31T14:00:00Z".parse().unwrap();
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", now).unwrap();
        let conn = db.lock().unwrap();
        let plan_date: String = conn
            .query_row("SELECT date FROM plans WHERE user_id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(plan_date, "2026-09-01");
        let debrief_date: String = conn
            .query_row("SELECT date FROM debriefs WHERE user_id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(debrief_date, "2026-09-01");
    }

    #[test]
    fn due_and_run_agree_on_the_evening_target_date() {
        let (db, tmp) = env("Asia/Tokyo", "22:00");
        let now: jiff::Timestamp = "2026-08-31T14:00:00Z".parse().unwrap();
        let d = due(&db.lock().unwrap(), tmp.path(), now).unwrap();
        assert_eq!(d, vec![(1, "aki".to_string())]);
        db.lock().unwrap().execute(
            "INSERT INTO debriefs (user_id, date, content, created_at) VALUES (1, '2026-09-01', 'x', 't')",
            [],
        ).unwrap();
        assert!(due(&db.lock().unwrap(), tmp.path(), now).unwrap().is_empty());
    }
```

- [ ] **Step 3: Run `cargo test` (existing 03:00 tests must stay green untouched), commit**

```bash
git add server/src/nightly.rs
git commit -m "fix: evening nightly runs target the next local date"
```

---

### Task 3: Identical repeated errors stop flooding the event log

A persistently broken user config logs an identical `runner_error` every 30s sweep and `nightly_config_error` every 60s. Add a throttled record variant that skips inserting when the most recent identical row is younger than the window, and use it at every recurring-error site. One-shot logs (`event_fired`, `delivery_*`, `login_error`, fallback logs) are untouched.

**Files:**
- Modify: `server/src/log.rs`
- Modify: `server/src/runner.rs` (fns `user_tz`, `fire_due`, `sweep_once`)
- Modify: `server/src/nightly.rs` (fn `due`, fn `spawn`)

**Interfaces:**
- Produces: `log::record_throttled(conn, user_id, kind, detail, now, window_mins) -> Result<bool>` — `Ok(true)` when a row was written, `Ok(false)` when suppressed.
- Consumes: `nightly::due`/`runner::fire_due` already receive `now`; `runner::user_tz` gains a `now: jiff::Timestamp` parameter (private fn, single caller).

- [ ] **Step 1: Add `record_throttled` to `server/src/log.rs`**

```rust
/// Writes the row unless an identical (user, kind, detail) row younger than
/// `window_mins` already exists — recurring failures (a broken user config
/// hit every sweep) log once per window instead of once per tick. Returns
/// whether a row was written.
pub fn record_throttled(
    conn: &Connection,
    user_id: Option<i64>,
    kind: &str,
    detail: &str,
    now: jiff::Timestamp,
    window_mins: i64,
) -> Result<bool> {
    let last: Option<String> = conn
        .query_row(
            "SELECT ts FROM event_log
             WHERE user_id IS ?1 AND kind = ?2 AND detail = ?3
             ORDER BY id DESC LIMIT 1",
            (user_id, kind, detail),
            |r| r.get(0),
        )
        .optional()?;
    if let Some(ts) = last.and_then(|s| s.parse::<jiff::Timestamp>().ok()) {
        if now.since(ts).is_ok_and(|span| span.get_seconds() < window_mins * 60) {
            return Ok(false);
        }
    }
    conn.execute(
        "INSERT INTO event_log (ts, user_id, kind, detail) VALUES (?1, ?2, ?3, ?4)",
        (now.to_string(), user_id, kind, detail),
    )?;
    Ok(true)
}
```

Add `use rusqlite::OptionalExtension;` to the file's imports. If `now.since(ts)` needs an explicit span unit to compare seconds, use `(now - ts).total(jiff::Unit::Second).is_ok_and(|s| s < (window_mins * 60) as f64)` instead — pick whichever compiles cleanly with the workspace's jiff version and assert the behavior via the tests below, not the arithmetic style.

- [ ] **Step 2: Unit tests in `server/src/log.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn rows(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM event_log", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn identical_errors_inside_the_window_collapse_to_one_row() {
        let conn = crate::db::open_memory().unwrap();
        let t0: jiff::Timestamp = "2026-08-31T00:00:00Z".parse().unwrap();
        let t1: jiff::Timestamp = "2026-08-31T00:30:00Z".parse().unwrap();
        assert!(record_throttled(&conn, None, "runner_error", "boom", t0, 60).unwrap());
        assert!(!record_throttled(&conn, None, "runner_error", "boom", t1, 60).unwrap());
        assert_eq!(rows(&conn), 1);
    }

    #[test]
    fn window_expiry_different_detail_and_different_user_all_write() {
        let conn = crate::db::open_memory().unwrap();
        let t0: jiff::Timestamp = "2026-08-31T00:00:00Z".parse().unwrap();
        let t2: jiff::Timestamp = "2026-08-31T01:00:01Z".parse().unwrap();
        assert!(record_throttled(&conn, None, "runner_error", "boom", t0, 60).unwrap());
        assert!(record_throttled(&conn, None, "runner_error", "other", t0, 60).unwrap());
        assert!(record_throttled(&conn, Some(1), "runner_error", "boom", t0, 60).unwrap());
        assert!(record_throttled(&conn, None, "runner_error", "boom", t2, 60).unwrap());
        assert_eq!(rows(&conn), 4);
    }
}
```

(`event_log.user_id` has no FK constraint enforced against users in-memory? It does — if inserting `Some(1)` violates a foreign key, create a user first in the test, mirroring how other log tests seed.)

- [ ] **Step 3: Switch the recurring sites, threading `now`**

All with a 60-minute window, as `const ERROR_LOG_WINDOW_MINS: i64 = 60;` in `log.rs` (pub):

- `runner.rs user_tz`: add `now: jiff::Timestamp` parameter; the unknown-timezone `record` becomes `record_throttled(conn, Some(c.user_id), "runner_error", &..., now, crate::log::ERROR_LOG_WINDOW_MINS)?` (discard the bool). Update its single call site in `fire_due` to pass `now`.
- `runner.rs fire_due`: the unresolvable-event `record` likewise becomes throttled.
- `runner.rs sweep_once`: both `runner_error` records (gc failure, fire_due failure) become throttled with the sweep's `now`.
- `nightly.rs due`: both `nightly_config_error` records become throttled with `now`.
- `nightly.rs spawn`: the `nightly_error` records (the `due` failure and the per-user failure) become throttled with the tick's `now`.

- [ ] **Step 4: Prove the flood stops at the runner level**

In `server/src/runner.rs` tests, extend `unresolvable_event_is_logged_and_skipped`: after the existing assertions, run `fire_due` again at `now + 30s` and assert the `runner_error` count is still 1:

```rust
        let again: jiff::Timestamp = "2026-08-31T12:00:30Z".parse().unwrap();
        fire_due(&conn, tmp.path(), again).unwrap();
        let errors: i64 = conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE kind='runner_error'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(errors, 1);
```

And in `nightly.rs`, extend `unreadable_user_config_is_skipped_and_logged` the same way: call `due` a second time 60 seconds later and assert `nightly_config_error` count stays 1.

- [ ] **Step 5: Run `cargo test`, commit**

```bash
git add server/src/log.rs server/src/runner.rs server/src/nightly.rs
git commit -m "fix: throttle identical recurring errors in the event log"
```

---

### Task 4: Template validation covers channel, window, and empty kind

`Template::validate` checks time/days/flexibility but lets through an unknown `channel` (which the delivery path treats as non-voice and delivers anyway), a negative `slide_window_min` (which makes every slide out-of-window), and an empty `kind`.

**Files:**
- Modify: `server/src/templates.rs`

- [ ] **Step 1: Extend `validate`**

Add after the `FLEXIBILITIES` const:

```rust
const CHANNELS: [&str; 2] = ["push", "voice"];
```

Add to the per-event checks in `validate` (after the flexibility check):

```rust
            if !CHANNELS.contains(&ev.channel.as_str()) {
                return Err(bad("channel", &ev.channel));
            }
            if ev.slide_window_min < 0 {
                return Err(bad("slide_window_min", &ev.slide_window_min.to_string()));
            }
            if ev.kind.trim().is_empty() {
                return Err(bad("kind", &ev.kind));
            }
```

- [ ] **Step 2: Tests**

```rust
    #[test]
    fn unknown_channel_negative_window_and_empty_kind_fail_to_load() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='09:00'\ndays=['mon']\nchannel='sms'\n");
        let err = Template::load(tmp.path(), "aki", "default").unwrap_err().to_string();
        assert!(err.contains("\"sms\""), "unexpected error: {err}");

        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind='nudge'\ntime='09:00'\ndays=['mon']\nslide_window_min=-5\n");
        let err = Template::load(tmp.path(), "aki", "default").unwrap_err().to_string();
        assert!(err.contains("slide_window_min"), "unexpected error: {err}");

        write(tmp.path(), "defaults/templates/default.toml",
            "[[events]]\nkind=''\ntime='09:00'\ndays=['mon']\n");
        assert!(Template::load(tmp.path(), "aki", "default").is_err());
    }
```

- [ ] **Step 3: Run `cargo test` — watch for existing tests or test fixtures using channels outside push/voice (none are known; `mock` channels appear only in `AppState` ladders, not templates). Commit**

```bash
git add server/src/templates.rs
git commit -m "fix: templates reject unknown channels, negative windows, empty kinds"
```

---

### Task 5: Real check-in copy

`render`'s check-in body is the placeholder `"{kind} at {wall_time}"` (e.g. "checkin_call at 09:00"), which leaks an internal identifier to the user's lock screen.

**Files:**
- Modify: `server/src/channels/mod.rs` (fn `render` and its test)

- [ ] **Step 1: Replace the check-in arm's body**

```rust
    let (title, mut body, urgency) = if ev.kind.contains("checkin") {
        (
            "Check-in".to_string(),
            format!("Time for your {} check-in — how is the day going?", ev.wall_time),
            Urgency::High,
        )
    } else if ev.kind == "debrief" {
```

- [ ] **Step 2: Pin it in `render_uses_debrief_content_and_message_override`**

After the existing check-in assertions add:

```rust
        assert!(m.body.contains("09:00"), "body: {}", m.body);
        assert!(!m.body.contains("checkin_call"), "body leaks the kind: {}", m.body);
```

- [ ] **Step 3: Run `cargo test` (the full-day and delivery-day suites must stay green — they assert titles and counts, not the check-in body), commit**

```bash
git add server/src/channels/mod.rs
git commit -m "fix: user-facing check-in copy instead of the event kind"
```

---

### Task 6: Talk and admin error-path polish

Three gaps in `api.rs`: an oversized/blank talk message returns a bare 400 with no body; a failed session collapses to a bare 500 with nothing in the event log; admin user creation burns an argon2 hash before validating the username.

**Files:**
- Modify: `server/src/api.rs` (fns `talk`, `admin_create_user`)
- Modify: `server/src/auth.rs` (make `validate_username` `pub(crate)`)
- Modify: `server/tests/common/mod.rs` (generalize the LLM parameter)
- Test: `server/tests/talk_api.rs`

**Interfaces:**
- Consumes: `auth::validate_username(&str) -> Result<()>` (currently private — change `fn` to `pub(crate) fn`, no other edits).
- Produces: `common::app_with_logged_in_user_and_llm` now takes `Arc<dyn LLMProvider>`; existing callers passing `Arc<MockLLM>` compile via unsize coercion — if any call site fails to coerce, wrap the argument as `llm as Arc<dyn LLMProvider>` at the call site.

- [ ] **Step 1: Talk request/response bodies**

In `talk`, replace the size check and the final match:

```rust
    if message.is_empty() || message.len() > MAX_TALK_MESSAGE {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "message must be 1..=16384 bytes" })),
        )
            .into_response();
    }
```

```rust
    match result {
        Ok(Ok(out)) => {
            let reply = if out.reply.trim().is_empty() {
                crate::EMPTY_REPLY_FALLBACK.to_string()
            } else {
                out.reply
            };
            Json(serde_json::json!({ "reply": reply })).into_response()
        }
        Ok(Err(e)) => {
            {
                let conn = state.db.lock().unwrap();
                let _ = crate::log::record(&conn, Some(user.id), "talk_error", &e.to_string());
            }
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": "the assistant is unavailable; try again" })),
            )
                .into_response()
        }
        Err(e) => {
            {
                let conn = state.db.lock().unwrap();
                let _ = crate::log::record(&conn, Some(user.id), "talk_error", &format!("talk task failed: {e}"));
            }
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
```

Note: `state` and `user` are moved into the `spawn_blocking` closure today. Capture what the error paths need before the move (`let err_db = state.db.clone(); let uid = user.id;`) and use `err_db`/`Some(uid)` in both error arms instead of `state.db`/`user.id`.

- [ ] **Step 2: Admin creation validates before hashing**

In `auth.rs`, change `fn validate_username` to `pub(crate) fn validate_username`. In `admin_create_user`'s closure:

```rust
    let created = tokio::task::spawn_blocking(move || {
        auth::validate_username(&req.username)?;
        let hash = auth::hash_password(&req.password)?;
        let conn = db.lock().unwrap();
        auth::insert_user(&conn, &req.username, &hash, req.admin)
    })
    .await;
```

- [ ] **Step 3: Generalize the test helper**

In `server/tests/common/mod.rs`, change the signature of `app_with_logged_in_user_and_llm` and `build` from `Arc<MockLLM>` / `Option<Arc<MockLLM>>` to `Arc<dyn note_server::providers::LLMProvider>` / `Option<Arc<dyn note_server::providers::LLMProvider>>` (adjust the `use` line accordingly; keep the `MockLLM` import only if still referenced).

- [ ] **Step 4: Tests in `server/tests/talk_api.rs`**

```rust
struct FailingLLM;
impl note_server::providers::LLMProvider for FailingLLM {
    fn chat(
        &self,
        _: &note_server::providers::ChatRequest,
    ) -> anyhow::Result<note_server::providers::ChatResponse> {
        anyhow::bail!("provider down")
    }
}

#[tokio::test]
async fn provider_failure_is_bad_gateway_and_logged() {
    let (app, cookie, _cfg) =
        common::app_with_logged_in_user_and_llm(std::sync::Arc::new(FailingLLM)).await;
    let res = talk(&app, &cookie, "hello").await;
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    let v: serde_json::Value = body_json(res).await;
    assert!(v["error"].as_str().unwrap().contains("unavailable"));
}

#[tokio::test]
async fn oversized_message_is_bad_request_with_body() {
    let (app, cookie, _cfg) = common::app_with_logged_in_user().await;
    let big = "a".repeat(16 * 1024 + 1);
    let res = talk(&app, &cookie, &big).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v: serde_json::Value = body_json(res).await;
    assert!(v["error"].as_str().unwrap().contains("16384"));
}
```

(`talk`/`body_json`: reuse the request helpers `talk_api.rs` already defines; if the log assertion for `talk_error` needs DB access, use `app_with_logged_in_user_and_state` with `with_providers` instead and assert `SELECT COUNT(*) FROM event_log WHERE kind='talk_error'` equals 1 after the 502.)

- [ ] **Step 5: Run `cargo test`, commit**

```bash
git add server/src/api.rs server/src/auth.rs server/tests/common/mod.rs server/tests/talk_api.rs
git commit -m "fix: talk error bodies and upstream status, admin validates before hashing"
```

---

## Out of scope

Ledgered as future work, deliberately untouched here: webpush consecutive-transport-failure pruning, ws keepalive configurability, fallback-debrief re-run, `secure_cookies` default (standing ruling: main.rs is the sole production constructor), per-IP rate limiting (reverse proxy's job), and the PWA (separate plan).
