use crate::agent::SessionDeps;
use crate::config::UserConfig;
use crate::text::Lang;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

/// How long past `brief_time` a brief may still be written: a server that was
/// down at that minute catches up, a time set in the evening waits for tomorrow.
const LATE_MIN: i64 = 4 * 60;
const MAX_NOTICE_CHARS: usize = 1000;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Brief {
    pub id: i64,
    pub date: String,
    pub content: String,
    pub created_at: String,
}

/// The brief written for `date`, if one was.
pub fn for_date(conn: &Connection, user_id: i64, date: &str) -> rusqlite::Result<Option<Brief>> {
    conn.query_row(
        "SELECT id, date, content, created_at FROM briefs
         WHERE user_id = ?1 AND date = ?2 AND content IS NOT NULL",
        (user_id, date),
        |r| Ok(Brief { id: r.get(0)?, date: r.get(1)?, content: r.get(2)?, created_at: r.get(3)? }),
    )
    .optional()
}

/// The work day a brief is owed for at `now`, whether or not it was written.
/// `brief_time` is a wall time on that work day, so one earlier than
/// `day_end` falls past its midnight; it is never owed once its day is over.
fn owed(ucfg: &UserConfig, now: jiff::Timestamp) -> Option<jiff::civil::Date> {
    let at = ucfg.brief_time();
    if !crate::templates::valid_time(at) {
        return None;
    }
    let clock = crate::zone::Clock::of(ucfg);
    let date = clock.today(now);
    let from = crate::zone::instant(&clock.tz, clock.civil_date(date, at), at)?;
    let late = from.checked_add(jiff::SignedDuration::from_mins(LATE_MIN)).ok()?;
    let (_, day_over) = clock.bounds(date).ok()?;
    (from <= now && now < late.min(day_over)).then_some(date)
}

/// Users whose brief is owed and not yet written, with the date it is for.
pub fn due(conn: &Connection, config_dir: &Path, now: jiff::Timestamp) -> Result<Vec<(i64, String, jiff::civil::Date)>> {
    let mut stmt = conn.prepare("SELECT id, username FROM users")?;
    let users: Vec<(i64, String)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for (id, username) in users {
        let Ok(ucfg) = UserConfig::load(config_dir, &username) else { continue };
        let Some(date) = owed(&ucfg, now) else { continue };
        let has: i64 = conn.query_row(
            "SELECT COUNT(*) FROM briefs WHERE user_id = ?1 AND date = ?2",
            (id, date.to_string()),
            |r| r.get(0),
        )?;
        if has == 0 {
            out.push((id, username, date));
        }
    }
    Ok(out)
}

/// Writes `date`'s brief. `None` when the user has done nothing since the last
/// one, which leaves the day open for when they come back, and when the
/// session failed, which closes it.
pub fn run_for_user(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    date: jiff::civil::Date,
    now: jiff::Timestamp,
) -> Result<Option<Brief>> {
    {
        let conn = crate::db_guard(deps.db);
        let last: Option<String> =
            conn.query_row("SELECT MAX(created_at) FROM briefs WHERE user_id = ?1", [user_id], |r| r.get(0))?;
        if crate::activity::quiet_since(&conn, user_id, last.and_then(|s| s.parse().ok()))? {
            crate::log::record_throttled(&conn, Some(user_id), "brief_quiet", &date.to_string(), now, 24 * 60)?;
            return Ok(None);
        }
    }
    let lang = Lang::for_user(deps.config_dir, username);
    let opening = crate::model_text::brief_opening(lang, date);
    let outcome = crate::agent::run_session(deps, user_id, username, crate::tools::SessionKind::Brief, now, &[], &opening);
    let content = match outcome {
        Ok(out) if !out.reply.trim().is_empty() => Some(out.reply.trim().to_string()),
        Ok(_) => {
            crate::log::record(&crate::db_guard(deps.db), Some(user_id), "brief_error", &format!("{date}: no reply"))?;
            None
        }
        Err(e) => {
            crate::failure::noted(&e);
            crate::log::record(&crate::db_guard(deps.db), Some(user_id), "brief_error", &format!("{date}: {e:#}"))?;
            None
        }
    };
    let conn = crate::db_guard(deps.db);
    conn.execute(
        "INSERT OR IGNORE INTO briefs (user_id, date, content, created_at) VALUES (?1, ?2, ?3, ?4)",
        (user_id, date.to_string(), content, now.to_string()),
    )?;
    Ok(for_date(&conn, user_id, &date.to_string())?)
}

/// The brief as a notification carries it: markdown marks dropped, lines kept.
pub fn plain(markdown: &str) -> String {
    let lines: Vec<String> = markdown
        .lines()
        .map(|line| {
            let line = line.trim_end();
            let line = line.trim_start_matches('#').trim_start_matches(' ');
            let line = match line.strip_prefix("* ") {
                Some(rest) => format!("- {rest}"),
                None => line.to_string(),
            };
            line.replace("**", "").replace("__", "").replace('`', "")
        })
        .collect();
    let mut text = lines.join("\n").split("\n\n\n").collect::<Vec<_>>().join("\n\n").trim().to_string();
    if text.chars().count() > MAX_NOTICE_CHARS {
        text = text.chars().take(MAX_NOTICE_CHARS - 1).chain(std::iter::once('…')).collect();
    }
    text
}

/// Tells the user their brief is there, unless a quiet window is holding
/// deliveries or they keep it to the app; the open app shows it either way.
fn announce(state: &crate::AppState, user_id: i64, username: &str, brief: &Brief, now: jiff::Timestamp) {
    state.hub.broadcast_changed(user_id);
    let quiet = UserConfig::load(&state.config_dir, username).is_ok_and(|ucfg| {
        let tz = crate::zone::named(&ucfg.timezone);
        ucfg.tuning.channel(crate::tuning::Category::Brief) == "chat"
            || crate::calendar::quiet_window(&state.db(), user_id, &tz, now).ok().flatten().is_some()
    });
    if quiet {
        return;
    }
    let lang = Lang::for_user(&state.config_dir, username);
    let msg = crate::channels::OutboundMessage {
        title: crate::text::brief_title(lang),
        reason: None,
        brief: None,
        body: plain(&brief.content),
        urgency: crate::channels::Urgency::Normal,
        checkin: false,
        event_id: None,
        conversation_id: None,
        actions: Vec::new(),
        wait: None,
        shown_in_app: true,
    };
    crate::channels::deliver_via(&state.db, &state.channels, user_id, username, &msg);
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
                    let _ = crate::log::record_throttled(
                        &state.db(),
                        None,
                        "brief_error",
                        &e.to_string(),
                        now,
                        crate::log::ERROR_LOG_WINDOW_MINS,
                    );
                    continue;
                }
            };
            for (user_id, username, date) in users {
                if crate::api::background_capped(&state, user_id, "brief") {
                    continue;
                }
                let st = state.clone();
                let ran = tokio::task::spawn_blocking(move || {
                    if st.skip_keyless(user_id, "brief") {
                        return;
                    }
                    let llm = st.llm_for(user_id);
                    let deps = SessionDeps {
                        db: &st.db,
                        config_dir: &st.config_dir,
                        data_dir: &st.data_dir,
                        llm: llm.as_ref(),
                        embeddings: st.embeddings.as_deref(),
                        search: st.search.as_deref(),
                        task_scope: None,
                        inbox_source: None,
                        memory_source: None,
                        token_id: None,
                        thread_note: None,
                        share: None,
                        caller: Some(&st),
                        conversation_id: None,
                    };
                    let (written, failures) = crate::failure::gather(|| run_for_user(&deps, user_id, &username, date, now));
                    for failure in &failures {
                        crate::failure::tell(&st, user_id, &username, failure, now);
                    }
                    match written {
                        Ok(Some(brief)) => announce(&st, user_id, &username, &brief, now),
                        Ok(None) => {}
                        Err(e) => {
                            let _ = crate::log::record_throttled(
                                &st.db(),
                                Some(user_id),
                                "brief_error",
                                &format!("{e:#}"),
                                now,
                                crate::log::ERROR_LOG_WINDOW_MINS,
                            );
                        }
                    }
                })
                .await;
                if let Err(join) = ran {
                    let _ = crate::log::record_throttled(
                        &state.db(),
                        Some(user_id),
                        "brief_error",
                        &format!("brief task panicked: {join}"),
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
    use crate::providers::{mock::MockLLM, ChatResponse};
    use std::sync::Mutex;

    fn env(brief_time: &str) -> (Mutex<Connection>, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        crate::auth::create_user(&conn, "aitest", "p", false).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let write = |rel: &str, c: &str| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        write("defaults/user.toml", "display_name = \"X\"\ntimezone = \"Asia/Tokyo\"\ntemplate = \"default\"\n");
        write("users/aitest/user.toml", &format!("brief_time = \"{brief_time}\"\n"));
        write("defaults/prompts/persona.md", "persona");
        write("defaults/prompts/brief.md", "write the brief");
        (Mutex::new(conn), tmp)
    }

    fn deps<'a>(
        db: &'a Mutex<Connection>,
        tmp: &'a tempfile::TempDir,
        llm: &'a dyn crate::providers::LLMProvider,
    ) -> SessionDeps<'a> {
        SessionDeps {
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
            caller: None,
            conversation_id: None,
        }
    }

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    fn said(text: &str) -> ChatResponse {
        ChatResponse { text: text.into(), tool_calls: vec![] }
    }

    #[test]
    fn a_brief_is_owed_from_its_time_for_four_hours_and_never_when_off() {
        let (db, tmp) = env("07:00");
        let conn = db.lock().unwrap();
        let due_at = |ts: &str| due(&conn, tmp.path(), at(ts)).unwrap();
        assert!(due_at("2026-11-25T21:59:00Z").is_empty(), "06:59 in Tokyo");
        assert_eq!(due_at("2026-11-25T22:00:00Z"), [(1, "aitest".to_string(), "2026-11-26".parse().unwrap())]);
        assert_eq!(due_at("2026-11-26T01:59:00Z").len(), 1, "10:59 still catches up");
        assert!(due_at("2026-11-26T02:00:00Z").is_empty(), "11:00 is too late");

        conn.execute(
            "INSERT INTO briefs (user_id, date, content, created_at) VALUES (1, '2026-11-26', NULL, 'x')",
            [],
        )
        .unwrap();
        assert!(due_at("2026-11-25T22:30:00Z").is_empty(), "a day tried once is not tried again");

        let mut ucfg = UserConfig::load(tmp.path(), "aitest").unwrap();
        ucfg.brief_time = Some(String::new());
        ucfg.save(tmp.path(), "aitest").unwrap();
        assert!(due_at("2026-11-26T22:00:00Z").is_empty(), "a blank time writes none");
    }

    #[test]
    fn a_brief_belongs_to_the_work_day_when_it_runs_past_midnight() {
        let (db, tmp) = env("07:00");
        let mut ucfg = UserConfig::load(tmp.path(), "aitest").unwrap();
        ucfg.tuning.day_end = Some("05:00".into());
        ucfg.save(tmp.path(), "aitest").unwrap();
        let conn = db.lock().unwrap();
        let due_at = |ts: &str| due(&conn, tmp.path(), at(ts)).unwrap();
        assert_eq!(due_at("2026-11-25T22:00:00Z"), [(1, "aitest".to_string(), "2026-11-26".parse().unwrap())]);
        assert!(due_at("2026-11-25T18:00:00Z").is_empty(), "03:00 is still the 25th's night");

        let mut ucfg = UserConfig::load(tmp.path(), "aitest").unwrap();
        ucfg.brief_time = Some("04:00".into());
        ucfg.save(tmp.path(), "aitest").unwrap();
        let owed_for = |ts: &str| due(&conn, tmp.path(), at(ts)).unwrap().first().map(|u| u.2.to_string());
        assert_eq!(owed_for("2026-11-25T19:00:00Z").as_deref(), Some("2026-11-25"), "04:00 on the 26th is the 25th's");
        assert_eq!(owed_for("2026-11-25T19:59:00Z").as_deref(), Some("2026-11-25"));
        assert_eq!(owed_for("2026-11-25T20:00:00Z"), None, "05:00 closes the 25th and its brief with it");
    }

    #[test]
    fn the_brief_is_written_once_read_back_by_date_and_skipped_after_a_quiet_day() {
        let (db, tmp) = env("07:00");
        let llm = MockLLM::scripted(vec![said("A light day.\n\n## Today\n- **essay** draft"), said("Back again.")]);
        let date: jiff::civil::Date = "2026-11-26".parse().unwrap();
        let brief = run_for_user(&deps(&db, &tmp, &llm), 1, "aitest", date, at("2026-11-25T22:00:00Z")).unwrap().unwrap();
        assert_eq!(brief.content, "A light day.\n\n## Today\n- **essay** draft");
        assert_eq!(brief.date, "2026-11-26");
        assert_eq!(llm.seen().len(), 1);
        assert!(llm.seen()[0].system.contains("write the brief"));
        assert_eq!(for_date(&db.lock().unwrap(), 1, "2026-11-26").unwrap(), Some(brief));

        let next: jiff::civil::Date = "2026-11-27".parse().unwrap();
        assert_eq!(run_for_user(&deps(&db, &tmp, &llm), 1, "aitest", next, at("2026-11-26T22:00:00Z")).unwrap(), None);
        assert_eq!(llm.seen().len(), 1, "nothing happened since the last brief");
        {
            let conn = db.lock().unwrap();
            assert_eq!(due(&conn, tmp.path(), at("2026-11-26T22:01:00Z")).unwrap().len(), 1, "the day stays open for a return");
            crate::presence::touch(&conn, 1, at("2026-11-26T23:00:00Z")).unwrap();
        }
        let back = run_for_user(&deps(&db, &tmp, &llm), 1, "aitest", next, at("2026-11-26T23:01:00Z")).unwrap().unwrap();
        assert_eq!(back.content, "Back again.");
    }

    #[test]
    fn a_failed_session_closes_the_day_without_a_brief() {
        let (db, tmp) = env("07:00");
        let llm = crate::providers::mock::NullLLM;
        let date: jiff::civil::Date = "2026-11-26".parse().unwrap();
        assert_eq!(run_for_user(&deps(&db, &tmp, &llm), 1, "aitest", date, at("2026-11-25T22:00:00Z")).unwrap(), None);
        let conn = db.lock().unwrap();
        assert_eq!(for_date(&conn, 1, "2026-11-26").unwrap(), None);
        assert!(due(&conn, tmp.path(), at("2026-11-25T22:05:00Z")).unwrap().is_empty());
        let logged: i64 =
            conn.query_row("SELECT COUNT(*) FROM event_log WHERE kind = 'brief_error'", [], |r| r.get(0)).unwrap();
        assert_eq!(logged, 1);
    }

    #[test]
    fn the_notice_drops_markdown_marks() {
        assert_eq!(
            plain("A light day.\n\n## Today\n* **essay** at `09:00`\n- call __mum__"),
            "A light day.\n\nToday\n- essay at 09:00\n- call mum"
        );
        let long = "x".repeat(MAX_NOTICE_CHARS + 10);
        assert_eq!(plain(&long).chars().count(), MAX_NOTICE_CHARS);
    }
}
