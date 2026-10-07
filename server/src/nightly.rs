use crate::agent::SessionDeps;
use crate::config::{Features, UserConfig};
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

/// The run's last step: yesterday's notes stay in place when it never happens.
const NOTES_TOOL: &str = "nightly_notes_write";

/// The date this nightly run plans and debriefs. An early-morning
/// `nightly_time` runs after midnight, so the current local date is the
/// sleeper's coming day; from noon onward the run precedes sleep and targets
/// the next date. Falls back to the current date if `tomorrow` overflows.
pub(crate) fn plan_date(local: &jiff::Zoned, nightly_time: &str) -> jiff::civil::Date {
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
            let _ = crate::log::record(&conn, Some(user_id), "memory_expire_error", &format!("{e:#}"));
        }
        if let Err(e) = crate::notes::purge_done(&conn, user_id, now) {
            let _ = crate::log::record(&conn, Some(user_id), "notes_purge_error", &format!("{e:#}"));
        }
    }
    let mut report = Report::new(date);
    let result = run_stages(deps, user_id, username, &ucfg, date, now, &mut report);
    {
        let conn = crate::db_guard(deps.db);
        let _ = crate::log::record(&conn, Some(user_id), "nightly_run", &report.line());
    }
    result
}

/// Every stage of one run, in order. Its caller writes the report whether this
/// returns or fails partway, so a run that dies still says where it died.
fn run_stages(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    ucfg: &UserConfig,
    date: jiff::civil::Date,
    now: jiff::Timestamp,
    report: &mut Report,
) -> Result<()> {
    let tz = jiff::tz::TimeZone::get(&ucfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    let note = |kind: &str, e: &anyhow::Error| {
        let conn = crate::db_guard(deps.db);
        let _ = crate::log::record_throttled(
            &conn,
            Some(user_id),
            kind,
            &format!("{e:#}"),
            now,
            crate::log::ERROR_LOG_WINDOW_MINS,
        );
    };
    // The day's conversations become memory before the plan is built, so the
    // planning session already counts what they left behind.
    let at = std::time::Instant::now();
    let harvested = crate::harvest::run_for_user(deps, user_id, username, &tz, date, now);
    report.stage("harvest", at, harvested.as_ref().err().inspect(|e| note("harvest_error", e)));
    // Monday's run looks back before it looks forward, so the planning session
    // already has the week's letter in memory.
    let at = std::time::Instant::now();
    let reviewed = crate::review::run_for_user(deps, user_id, username, &tz, date, now);
    report.stage("review", at, reviewed.as_ref().err().inspect(|e| note("review_error", e)));
    let at = std::time::Instant::now();
    let learned = {
        let conn = crate::db_guard(deps.db);
        crate::learn::run_for_user(&conn, user_id, now)
    };
    report.stage("learn", at, learned.as_ref().err().inspect(|e| note("learn_error", e)));

    let at = std::time::Instant::now();
    let generated = (|| -> Result<()> {
        let conn = crate::db_guard(deps.db);
        let tmpl = crate::templates::Template::load(deps.config_dir, username, &ucfg.template)?;
        crate::plan::generate(&conn, user_id, &tmpl, date)?;
        Ok(())
    })();
    report.stage("plan", at, generated.as_ref().err());
    generated?;
    let at = std::time::Instant::now();
    let allocated = {
        let conn = crate::db_guard(deps.db);
        if let Err(e) = crate::review::lay_event(&conn, user_id, date) {
            note("review_error", &e);
        }
        crate::allocate::run(&conn, user_id, &tz, date, now)
    };
    report.stage("allocate", at, allocated.as_ref().err().inspect(|e| note("allocate_error", e)));
    let at = std::time::Instant::now();
    let closed = {
        let conn = crate::db_guard(deps.db);
        crate::triggers::lay_close_day(&conn, deps.config_dir, username, user_id, date, now)
    };
    report.stage("close_day", at, closed.as_ref().err().inspect(|e| note("close_day_error", e)));

    let at = std::time::Instant::now();
    let outcome = crate::agent::run_session(
        deps,
        user_id,
        username,
        crate::tools::SessionKind::Nightly,
        now,
        &[],
        &crate::model_text::nightly_opening(crate::text::Lang::for_user(deps.config_dir, username), date),
    );
    report.stage("session", at, outcome.as_ref().err());
    let fallback = crate::text::fallback_debrief(crate::text::Lang::for_user(deps.config_dir, username));
    let content = match outcome {
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
            if out.reply.trim().is_empty() { fallback.clone() } else { out.reply }
        }
        Err(e) => {
            let conn = crate::db_guard(deps.db);
            let _ = crate::log::record(&conn, Some(user_id), "nightly_fallback", &format!("{e:#}"));
            fallback.clone()
        }
    };
    report.debrief(content != fallback);
    let conn = crate::db_guard(deps.db);
    conn.execute(
        "INSERT OR IGNORE INTO debriefs (user_id, date, content, created_at) VALUES (?1, ?2, ?3, ?4)",
        (user_id, date.to_string(), content, now.to_string()),
    )?;
    Ok(())
}

/// What the run did, stage by stage, for the one `nightly_run` row it leaves.
/// Stage names and timings only: the errors themselves have their own rows and
/// this one is readable on a release build.
struct Report {
    date: jiff::civil::Date,
    stages: Vec<String>,
    debrief: Option<&'static str>,
}

impl Report {
    fn new(date: jiff::civil::Date) -> Self {
        Self { date, stages: Vec::new(), debrief: None }
    }

    fn stage(&mut self, name: &str, at: std::time::Instant, failure: Option<&anyhow::Error>) {
        let status = match failure {
            Some(_) => "error",
            None => "ok",
        };
        self.stages.push(format!("{name}={status} {:.1}s", at.elapsed().as_secs_f64()));
    }

    fn debrief(&mut self, written: bool) {
        self.debrief = Some(if written { "written" } else { "fallback" });
    }

    fn line(&self) -> String {
        let mut parts = self.stages.clone();
        if let Some(d) = self.debrief {
            parts.push(format!("debrief={d}"));
        }
        format!("{}: {}", self.date, parts.join(", "))
    }
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
                        search: st.search.as_deref(),
                        task_scope: None,
                        inbox_source: None,
                        memory_source: None,
                        token_id: None,
                        thread_note: None,
                        share: None,
                    };
                    let r = run_for_user(&deps, user_id, &username, now);
                    (r, username)
                })
                .await;
                let failure = match result {
                    Ok((Ok(()), _)) => None,
                    Ok((Err(e), username)) => Some(format!("{username}: {e:#}")),
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
        write("defaults/prompts/harvest.md", "harvest");
        write("defaults/prompts/review.md", "read the week");
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
            search: None,
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            token_id: None,
            thread_note: None,
            share: None,
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
            assert_eq!(n, 2, "the template event and the close of the day");
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
            2
        );
        assert_eq!(
            counts("SELECT COUNT(*) FROM debriefs WHERE user_id=1 AND date='2026-08-31'"),
            1
        );
    }

    fn close_day_rows(conn: &rusqlite::Connection) -> Vec<(String, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT e.wall_time, e.prompt FROM events e JOIN plans p ON p.id = e.plan_id
                 WHERE p.date = '2026-08-31' AND e.kind = 'trigger' AND e.origin = 'template'",
            )
            .unwrap();
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        rows.collect::<rusqlite::Result<_>>().unwrap()
    }

    #[test]
    fn the_close_of_the_day_is_laid_once_per_date() {
        let (db, tmp) = env("UTC", "03:00");
        let llm = MockLLM::scripted(vec![ChatResponse { text: "ok".into(), tool_calls: vec![] }]);
        let now: jiff::Timestamp = "2026-08-31T04:00:00Z".parse().unwrap();
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", now).unwrap();
        {
            let conn = db.lock().unwrap();
            let rows = close_day_rows(&conn);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].0, crate::config::DEFAULT_CLOSE_DAY_TIME);
            assert!(rows[0].1.contains("plan_carry"));
            conn.execute("DELETE FROM debriefs", []).unwrap();
        }
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", now).unwrap();
        let conn = db.lock().unwrap();
        assert_eq!(close_day_rows(&conn).len(), 1);
        let spent = crate::triggers::spent(&conn, 1, "2026-08-31".parse().unwrap()).unwrap();
        assert_eq!(spent, 0, "a system check never costs the day's budget");
    }

    #[test]
    fn a_blank_close_day_time_lays_nothing() {
        let (db, tmp) = env("UTC", "03:00");
        let path = tmp.path().join("defaults/user.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{raw}close_day_time = \"\"\n")).unwrap();
        let llm = MockLLM::scripted(vec![ChatResponse { text: "ok".into(), tool_calls: vec![] }]);
        let now: jiff::Timestamp = "2026-08-31T04:00:00Z".parse().unwrap();
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", now).unwrap();
        let conn = db.lock().unwrap();
        assert!(close_day_rows(&conn).is_empty());
    }

    #[test]
    fn a_monday_run_writes_the_week_and_lays_it_beside_the_debrief() {
        let (db, tmp) = env("UTC", "03:00");
        std::fs::write(
            tmp.path().join("defaults/templates/default.toml"),
            "[[events]]\nkind='debrief'\ntime='07:30'\ndays=['mon']\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("defaults/prompts/review.md"), "read the week").unwrap();
        db.lock()
            .unwrap()
            .execute(
                "INSERT INTO tasks (user_id, title, state, created_at, updated_at, completed_at)
                 VALUES (1, 'the essay', 'done', 'x', '2026-09-16T04:00:00Z', '2026-09-16T04:00:00Z')",
                [],
            )
            .unwrap();
        let llm = MockLLM::scripted(vec![
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "review_write".into(),
                    args: r#"{"text":"You finished the essay."}"#.into(),
                }],
            },
            ChatResponse { text: "good morning".into(), tool_calls: vec![] },
        ]);
        let now: jiff::Timestamp = "2026-09-21T04:00:00Z".parse().unwrap();
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", now).unwrap();

        let conn = db.lock().unwrap();
        let content: String = conn
            .query_row(
                "SELECT content FROM reviews WHERE user_id = 1 AND week_start = '2026-09-14'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(content, "You finished the essay.");
        let (kind, wall): (String, String) = conn
            .query_row(
                "SELECT e.kind, e.wall_time FROM events e JOIN plans p ON p.id = e.plan_id
                 WHERE p.date = '2026-09-21' AND e.kind = 'review'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((kind.as_str(), wall.as_str()), ("review", "07:31"));
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
        assert_eq!(content, crate::text::fallback_debrief(crate::text::Lang::En));
        let logged: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='nightly_fallback'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(logged, 0);
    }

    fn nightly_run_row(conn: &rusqlite::Connection) -> String {
        conn.query_row("SELECT detail FROM event_log WHERE kind = 'nightly_run'", [], |r| r.get(0))
            .unwrap()
    }

    /// The report line with each stage's seconds dropped, so it can be compared.
    fn without_timings(line: &str) -> String {
        line.split(", ")
            .map(|part| match part.rsplit_once(' ') {
                Some((head, secs)) if secs.ends_with('s') => head,
                _ => part,
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    #[test]
    fn a_finished_run_reports_every_stage_it_went_through() {
        let (db, tmp) = env("UTC", "03:00");
        let llm = MockLLM::scripted(vec![ChatResponse {
            text: "good morning".into(),
            tool_calls: vec![],
        }]);
        let now: jiff::Timestamp = "2026-08-31T04:00:00Z".parse().unwrap();
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", now).unwrap();
        let line = nightly_run_row(&db.lock().unwrap());
        assert_eq!(
            without_timings(&line),
            "2026-08-31: harvest=ok, review=ok, learn=ok, plan=ok, allocate=ok, close_day=ok, \
             session=ok, debrief=written"
        );
        assert_ne!(line, without_timings(&line), "every stage carries its own seconds");
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
        let line = nightly_run_row(&conn);
        assert!(line.contains("session=error"), "{line}");
        assert!(line.ends_with("debrief=fallback"), "{line}");
        assert!(!line.contains("down"), "the cause has its own row, not this one");
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
    #[test]
    fn the_nightly_run_deletes_notes_done_over_a_week_ago() {
        let (db, tmp) = env("UTC", "03:00");
        let llm = MockLLM::scripted(vec![ChatResponse { text: "ok".into(), tool_calls: vec![] }]);
        {
            let conn = db.lock().unwrap();
            for (text, done) in [
                ("open", None),
                ("done lately", Some("2026-08-24T04:00:00Z")),
                ("done long ago", Some("2026-08-24T03:59:59Z")),
            ] {
                conn.execute(
                    "INSERT INTO notes (user_id, text, created_at, done_at)
                     VALUES (1, ?1, '2026-08-01T00:00:00Z', ?2)",
                    (text, done),
                )
                .unwrap();
            }
        }
        let now: jiff::Timestamp = "2026-08-31T04:00:00Z".parse().unwrap();
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", now).unwrap();

        let conn = db.lock().unwrap();
        let mut stmt = conn.prepare("SELECT text FROM notes ORDER BY id").unwrap();
        let left: Vec<String> =
            stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(left, ["open", "done lately"]);
    }
}
