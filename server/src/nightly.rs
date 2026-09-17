use crate::agent::SessionDeps;
use crate::config::{Features, UserConfig};
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

/// The run's last step: yesterday's notes stay in place when it never happens.
const NOTES_TOOL: &str = "nightly_notes_write";

const FALLBACK_DEBRIEF: &str =
    "(Plan generated from your template. The assistant was unavailable overnight.)";

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

/// One user's nightly run: the template plan is generated in pure code before
/// the agent is involved, so a provider outage still leaves a usable morning
/// plan. The debrief row doubles as the idempotency marker, which is why a
/// failed session writes the fallback text instead of retrying every sweep.
pub fn run_for_user(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    now: jiff::Timestamp,
) -> Result<()> {
    let ucfg = UserConfig::load(deps.config_dir, username)?;
    let tz = jiff::tz::TimeZone::get(&ucfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    let local = now.to_zoned(tz);
    let date = plan_date(&local, &ucfg.nightly_time);
    {
        let conn = crate::db_guard(deps.db);
        let done: i64 = conn.query_row(
            "SELECT COUNT(*) FROM debriefs WHERE user_id = ?1 AND date = ?2",
            (user_id, date.to_string()),
            |r| r.get(0),
        )?;
        if done > 0 {
            return Ok(());
        }
        if let Err(e) = crate::memory::archive_expired(&conn, deps.data_dir, username, local.date()) {
            let _ = crate::log::record(&conn, Some(user_id), "memory_expire_error", &e.to_string());
        }
        let tmpl = crate::templates::Template::load(deps.config_dir, username, &ucfg.template)?;
        crate::plan::generate(&conn, user_id, &tmpl, date)?;
    }
    let content = match crate::agent::run_session(
        deps,
        user_id,
        username,
        crate::tools::SessionKind::Nightly,
        now,
        &[],
        &format!("Nightly run for {date}."),
    ) {
        Ok(out) => {
            if !out.steps.iter().any(|s| s.name == NOTES_TOOL && !s.is_error) {
                let conn = crate::db_guard(deps.db);
                let _ = crate::log::record_throttled(
                    &conn,
                    Some(user_id),
                    "nightly_notes_missing",
                    &format!("no notes written for {date}"),
                    now,
                    crate::log::ERROR_LOG_WINDOW_MINS,
                );
            }
            match out.reply.trim().is_empty() {
                true => FALLBACK_DEBRIEF.to_string(),
                false => out.reply,
            }
        }
        Err(e) => {
            let conn = crate::db_guard(deps.db);
            let _ = crate::log::record(&conn, Some(user_id), "nightly_fallback", &e.to_string());
            FALLBACK_DEBRIEF.to_string()
        }
    };
    let conn = crate::db_guard(deps.db);
    conn.execute(
        "INSERT OR IGNORE INTO debriefs (user_id, date, content, created_at) VALUES (?1, ?2, ?3, ?4)",
        (user_id, date.to_string(), content, now.to_string()),
    )?;
    Ok(())
}

/// Users whose local time has reached their configured `nightly_time` and who
/// have no debrief for that run's target date yet. A user whose config cannot be read
/// is skipped rather than failing the whole sweep, but the skip is logged: an
/// unreadable config otherwise ends that user's nightlies silently and forever.
pub fn due(
    conn: &Connection,
    config_dir: &Path,
    now: jiff::Timestamp,
) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare("SELECT id, username, category FROM users")?;
    let users: Vec<(i64, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for (id, username, category) in users {
        let ucfg = match UserConfig::load(config_dir, &username) {
            Ok(c) => c,
            Err(_) if !Features::for_category(&category).nightly => continue,
            Err(e) => {
                let _ = crate::log::record_throttled(
                    conn,
                    Some(id),
                    "nightly_config_error",
                    &e.to_string(),
                    now,
                    crate::log::ERROR_LOG_WINDOW_MINS,
                );
                continue;
            }
        };
        if !ucfg.features(&category).nightly {
            continue;
        }
        let tz = jiff::tz::TimeZone::get(&ucfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
        let local = now.to_zoned(tz);
        let Ok(due_time) = format!("{}:00", ucfg.nightly_time).parse::<jiff::civil::Time>() else {
            let _ = crate::log::record_throttled(
                conn,
                Some(id),
                "nightly_config_error",
                &format!("unparseable nightly_time {:?}", ucfg.nightly_time),
                now,
                crate::log::ERROR_LOG_WINDOW_MINS,
            );
            continue;
        };
        if local.time() < due_time {
            continue;
        }
        let target = plan_date(&local, &ucfg.nightly_time);
        let has: i64 = conn.query_row(
            "SELECT COUNT(*) FROM debriefs WHERE user_id = ?1 AND date = ?2",
            (id, target.to_string()),
            |r| r.get(0),
        )?;
        if has == 0 {
            out.push((id, username));
        }
    }
    Ok(out)
}

pub fn spawn(state: crate::AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tick.tick().await;
            let now = jiff::Timestamp::now();
            let users = {
                let conn = state.db();
                due(&conn, &state.config_dir, now)
            };
            let users = match users {
                Ok(u) => u,
                Err(e) => {
                    let conn = state.db();
                    let _ = crate::log::record_throttled(
                        &conn,
                        None,
                        "nightly_error",
                        &e.to_string(),
                        now,
                        crate::log::ERROR_LOG_WINDOW_MINS,
                    );
                    continue;
                }
            };
            for (user_id, username) in users {
                let st = state.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let deps = SessionDeps {
                        db: &st.db,
                        config_dir: &st.config_dir,
                        data_dir: &st.data_dir,
                        llm: st.llm.as_ref(),
                        embeddings: st.embeddings.as_deref(),
                        task_scope: None,
                        inbox_source: None,
                        token_id: None,
                        thread_note: None,
                    };
                    let r = run_for_user(&deps, user_id, &username, now);
                    (r, username)
                })
                .await;
                let failure = match result {
                    Ok((Ok(()), _)) => None,
                    Ok((Err(e), username)) => Some(format!("{username}: {e}")),
                    Err(join) => Some(format!("nightly task panicked: {join}")),
                };
                if let Some(detail) = failure {
                    let conn = state.db();
                    let _ = crate::log::record_throttled(
                        &conn,
                        Some(user_id),
                        "nightly_error",
                        &detail,
                        now,
                        crate::log::ERROR_LOG_WINDOW_MINS,
                    );
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{mock::MockLLM, ChatResponse, ToolCall};
    use std::sync::Mutex;

    fn env(tz: &str, nightly_time: &str) -> (Mutex<rusqlite::Connection>, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let write = |rel: &str, c: &str| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        write(
            "defaults/user.toml",
            &format!(
                "display_name = \"X\"\ntimezone = \"{tz}\"\ntemplate = \"default\"\nnightly_time = \"{nightly_time}\"\n"
            ),
        );
        write(
            "defaults/templates/default.toml",
            "[[events]]\nkind='checkin'\ntime='09:00'\ndays=['mon','tue','wed','thu','fri','sat','sun']\nflexibility='slide'\n",
        );
        write("defaults/prompts/persona.md", "persona");
        write("defaults/prompts/planning.md", "planning");
        (Mutex::new(conn), tmp)
    }

    fn deps<'a>(
        db: &'a Mutex<rusqlite::Connection>,
        tmp: &'a tempfile::TempDir,
        llm: &'a dyn crate::providers::LLMProvider,
    ) -> crate::agent::SessionDeps<'a> {
        crate::agent::SessionDeps {
            db,
            config_dir: tmp.path(),
            data_dir: tmp.path(),
            llm,
            embeddings: None,
            task_scope: None,
            inbox_source: None,
            token_id: None,
            thread_note: None,
        }
    }

    #[test]
    fn nightly_generates_plan_and_stores_debrief_idempotently() {
        let (db, tmp) = env("UTC", "03:00");
        let llm = MockLLM::scripted(vec![
            ChatResponse { text: "good morning! light day ahead".into(), tool_calls: vec![] },
            ChatResponse { text: "should never be consumed".into(), tool_calls: vec![] },
        ]);
        let now: jiff::Timestamp = "2026-08-31T04:00:00Z".parse().unwrap();
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", now).unwrap();
        {
            let conn = db.lock().unwrap();
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM events e JOIN plans p ON p.id = e.plan_id WHERE p.date='2026-08-31'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1);
            let content: String = conn
                .query_row(
                    "SELECT content FROM debriefs WHERE user_id=1 AND date='2026-08-31'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(content.contains("light day"));
        }
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", now).unwrap();
        assert_eq!(llm.seen().len(), 1);
        let conn = db.lock().unwrap();
        let counts = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(counts("SELECT COUNT(*) FROM plans WHERE date='2026-08-31'"), 1);
        assert_eq!(
            counts(
                "SELECT COUNT(*) FROM events e JOIN plans p ON p.id = e.plan_id WHERE p.date='2026-08-31'"
            ),
            1
        );
        assert_eq!(
            counts("SELECT COUNT(*) FROM debriefs WHERE user_id=1 AND date='2026-08-31'"),
            1
        );
    }

    #[test]
    fn blank_reply_stores_fallback_without_logging_a_failure() {
        let (db, tmp) = env("UTC", "03:00");
        let llm = MockLLM::scripted(vec![ChatResponse { text: "   \n".into(), tool_calls: vec![] }]);
        let now: jiff::Timestamp = "2026-08-31T04:00:00Z".parse().unwrap();
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", now).unwrap();
        let conn = db.lock().unwrap();
        let content: String = conn
            .query_row("SELECT content FROM debriefs WHERE user_id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(content, FALLBACK_DEBRIEF);
        let logged: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='nightly_fallback'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(logged, 0);
    }

    #[test]
    fn llm_failure_still_leaves_plan_and_fallback_debrief() {
        struct Failing;
        impl crate::providers::LLMProvider for Failing {
            fn chat(
                &self,
                _: &crate::providers::ChatRequest,
            ) -> anyhow::Result<crate::providers::ChatResponse> {
                anyhow::bail!("down")
            }
        }
        let (db, tmp) = env("UTC", "03:00");
        let now: jiff::Timestamp = "2026-08-31T04:00:00Z".parse().unwrap();
        run_for_user(&deps(&db, &tmp, &Failing), 1, "aki", now).unwrap();
        let conn = db.lock().unwrap();
        let plans: i64 = conn.query_row("SELECT COUNT(*) FROM plans", [], |r| r.get(0)).unwrap();
        assert_eq!(plans, 1);
        let content: String = conn
            .query_row("SELECT content FROM debriefs WHERE user_id=1", [], |r| r.get(0))
            .unwrap();
        assert!(content.contains("template"));
        let logged: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='nightly_fallback'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(logged, 1);
    }

    #[test]
    fn unreadable_user_config_is_skipped_and_logged() {
        let (db, tmp) = env("UTC", "03:00");
        std::fs::write(tmp.path().join("defaults/user.toml"), "nightly_time = \"25:99\"\n").unwrap();
        let now: jiff::Timestamp = "2026-08-31T04:00:00Z".parse().unwrap();
        let conn = db.lock().unwrap();
        assert!(due(&conn, tmp.path(), now).unwrap().is_empty());
        let logged: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='nightly_config_error'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(logged, 1);

        let later: jiff::Timestamp = "2026-08-31T04:01:00Z".parse().unwrap();
        assert!(due(&conn, tmp.path(), later).unwrap().is_empty());
        let logged: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='nightly_config_error'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(logged, 1);
    }

    #[test]
    fn a_test_account_has_no_nightly_until_it_is_switched_on() {
        let (db, tmp) = env("Asia/Tokyo", "03:00");
        let now: jiff::Timestamp = "2026-08-30T19:00:00Z".parse().unwrap();
        {
            let conn = db.lock().unwrap();
            crate::auth::set_category(&conn, "aki", "test").unwrap();
            assert!(due(&conn, tmp.path(), now).unwrap().is_empty());
            let logged: i64 = conn
                .query_row("SELECT COUNT(*) FROM event_log", [], |r| r.get(0))
                .unwrap();
            assert_eq!(logged, 0, "a skipped account costs nothing, not even a log row");
        }

        std::fs::create_dir_all(tmp.path().join("users/aki")).unwrap();
        std::fs::write(
            tmp.path().join("users/aki/user.toml"),
            "nightly_enabled = true\n",
        )
        .unwrap();
        let conn = db.lock().unwrap();
        assert_eq!(due(&conn, tmp.path(), now).unwrap(), vec![(1, "aki".to_string())]);
    }

    #[test]
    fn due_respects_local_time_and_existing_debriefs() {
        let (db, tmp) = env("Asia/Tokyo", "03:00");
        let early: jiff::Timestamp = "2026-08-30T17:00:00Z".parse().unwrap();
        assert!(due(&db.lock().unwrap(), tmp.path(), early).unwrap().is_empty());
        let later: jiff::Timestamp = "2026-08-30T19:00:00Z".parse().unwrap();
        let d = due(&db.lock().unwrap(), tmp.path(), later).unwrap();
        assert_eq!(d, vec![(1, "aki".to_string())]);
        db.lock()
            .unwrap()
            .execute(
                "INSERT INTO debriefs (user_id, date, content, created_at) VALUES (1, '2026-08-31', 'x', 't')",
                [],
            )
            .unwrap();
        assert!(due(&db.lock().unwrap(), tmp.path(), later).unwrap().is_empty());
    }

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

    fn missing_rows(conn: &rusqlite::Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE kind='nightly_notes_missing'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn the_nightly_run_leaves_notes_for_tomorrow() {
        let (db, tmp) = env("UTC", "03:00");
        let llm = MockLLM::scripted(vec![
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "nightly_notes_write".into(),
                    args: r#"{"text":"the essay is the one that matters"}"#.into(),
                }],
            },
            ChatResponse { text: "good morning! light day ahead".into(), tool_calls: vec![] },
        ]);
        let now: jiff::Timestamp = "2026-08-31T04:00:00Z".parse().unwrap();
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", now).unwrap();

        let notes =
            std::fs::read_to_string(crate::context::nightly_notes_path(tmp.path(), "aki")).unwrap();
        assert!(notes.contains("the essay is the one that matters"), "{notes}");
        assert_eq!(missing_rows(&db.lock().unwrap()), 0);
    }

    #[test]
    fn a_night_that_writes_no_notes_is_logged_and_keeps_the_old_ones() {
        let (db, tmp) = env("UTC", "03:00");
        crate::context::write_nightly_notes(
            tmp.path(),
            "aki",
            "still the essay",
            "2026-08-30".parse().unwrap(),
        )
        .unwrap();
        let llm = MockLLM::scripted(vec![
            ChatResponse { text: "good morning! light day ahead".into(), tool_calls: vec![] },
        ]);
        let now: jiff::Timestamp = "2026-08-31T04:00:00Z".parse().unwrap();
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", now).unwrap();

        assert_eq!(
            std::fs::read_to_string(crate::context::nightly_notes_path(tmp.path(), "aki")).unwrap(),
            "<!-- written 2026-08-30 -->\nstill the essay\n",
        );
        assert_eq!(missing_rows(&db.lock().unwrap()), 1);
    }
}
