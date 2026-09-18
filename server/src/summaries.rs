use crate::agent::SessionDeps;
use crate::config::{Features, UserConfig};
use crate::tools::SessionKind;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;

/// Conversations summarised per tick, and how many rows the query looks at to
/// find them: a sweep of accounts that summarise nothing must not starve the
/// ones that do.
const BATCH: usize = 4;
const SCAN: usize = 64;
const HISTORY_LIMIT: usize = 64;
/// Consecutive failures after which a conversation is left alone for a while.
const MAX_FAILURES: u32 = 3;
const BACKOFF_MINS: i64 = 60;

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub conversation_id: i64,
    pub user_id: i64,
    pub username: String,
    /// The summary the conversation already carries, if any.
    pub summary: Option<String>,
    pub summary_through: i64,
    /// The last row the new summary will cover.
    pub through: i64,
}

/// Conversations quiet since `cutoff` that hold user turns no summary covers.
/// A user whose nightly is off is skipped, so a test account stays silent.
pub fn due(conn: &Connection, config_dir: &Path, cutoff: jiff::Timestamp) -> Result<Vec<Candidate>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.user_id, u.username, u.category, c.summary,
                COALESCE(c.summary_through, 0),
                (SELECT MAX(m.id) FROM talk_messages m WHERE m.conversation_id = c.id)
         FROM conversations c JOIN users u ON u.id = c.user_id
         WHERE c.updated_at <= ?1
           AND EXISTS (SELECT 1 FROM talk_messages m WHERE m.conversation_id = c.id
                       AND m.role = 'user' AND m.id > COALESCE(c.summary_through, 0))
         ORDER BY c.updated_at LIMIT ?2",
    )?;
    let rows = stmt
        .query_map((cutoff.to_string(), SCAN as i64), |r| {
            Ok((
                Candidate {
                    conversation_id: r.get(0)?,
                    user_id: r.get(1)?,
                    username: r.get(2)?,
                    summary: r.get(4)?,
                    summary_through: r.get(5)?,
                    through: r.get(6)?,
                },
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut summarises: HashMap<i64, bool> = HashMap::new();
    let mut out = Vec::new();
    for (candidate, category) in rows {
        let on = *summarises.entry(candidate.user_id).or_insert_with(|| {
            match UserConfig::load(config_dir, &candidate.username) {
                Ok(cfg) => cfg.features(&category).nightly,
                Err(_) => Features::for_category(&category).nightly,
            }
        });
        if on {
            out.push(candidate);
        }
        if out.len() == BATCH {
            break;
        }
    }
    Ok(out)
}

/// Summarises one conversation and stores the result. The session replays only
/// the turns after the summary the conversation already carries.
pub fn run_for_conversation(
    deps: &SessionDeps,
    candidate: &Candidate,
    now: jiff::Timestamp,
) -> Result<()> {
    let history = {
        let conn = crate::db_guard(deps.db);
        crate::talk::history_after(
            &conn,
            candidate.conversation_id,
            candidate.summary_through,
            HISTORY_LIMIT,
        )?
    };
    let opening = match &candidate.summary {
        Some(s) => format!("Summary so far: {s}\n\nSummarise this conversation."),
        None => "Summarise this conversation.".to_string(),
    };
    let out = crate::agent::run_session(
        deps,
        candidate.user_id,
        &candidate.username,
        SessionKind::Summarize,
        now,
        &history,
        &opening,
    )?;
    let summary = out
        .steps
        .iter()
        .rev()
        .find(|s| s.name == "summary_write" && !s.is_error)
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s.result).ok())
        .and_then(|v| v["summary"].as_str().map(str::to_string))
        .ok_or_else(|| anyhow::anyhow!("the session wrote no summary"))?;
    let conn = crate::db_guard(deps.db);
    crate::talk::store_summary(&conn, candidate.conversation_id, &summary, candidate.through, now)?;
    Ok(())
}

#[derive(Default)]
struct Failures {
    consecutive: u32,
    until: Option<jiff::Timestamp>,
}

/// Retries a conversation that keeps failing at a slower pace, so a thread the
/// model chokes on cannot spend a session every minute.
fn backed_off(failures: &HashMap<i64, Failures>, id: i64, now: jiff::Timestamp) -> bool {
    failures.get(&id).and_then(|f| f.until).is_some_and(|until| now < until)
}

fn note_failure(failures: &mut HashMap<i64, Failures>, id: i64, now: jiff::Timestamp) {
    let entry = failures.entry(id).or_default();
    entry.consecutive += 1;
    if entry.consecutive >= MAX_FAILURES {
        entry.consecutive = 0;
        entry.until = now.checked_add(jiff::Span::new().minutes(BACKOFF_MINS)).ok();
    }
}

/// The idle-summary pass: every minute, the conversations that have gone quiet
/// get the summary they will be remembered by. A zero threshold turns the pass
/// off entirely.
pub fn spawn(state: crate::AppState) {
    let idle = state.idle_summary_min;
    if idle == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        let mut failures: HashMap<i64, Failures> = HashMap::new();
        loop {
            tick.tick().await;
            let now = jiff::Timestamp::now();
            let Some(cutoff) = now.checked_sub(jiff::Span::new().minutes(idle as i64)).ok() else {
                continue;
            };
            let candidates = {
                let conn = state.db();
                due(&conn, &state.config_dir, cutoff)
            };
            let candidates = match candidates {
                Ok(c) => c,
                Err(e) => {
                    let conn = state.db();
                    let _ = crate::log::record_throttled(
                        &conn,
                        None,
                        "summary_error",
                        &e.to_string(),
                        now,
                        crate::log::ERROR_LOG_WINDOW_MINS,
                    );
                    continue;
                }
            };
            for candidate in candidates {
                if backed_off(&failures, candidate.conversation_id, now) {
                    continue;
                }
                // A user already in a session keeps it; the conversation is
                // still quiet on the next tick.
                let Ok(permit) = state.talk_gate.try_enter(candidate.user_id) else { continue };
                let st = state.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
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
                    };
                    let r = run_for_conversation(&deps, &candidate, now);
                    (r, candidate)
                })
                .await;
                let (id, failure) = match result {
                    Ok((Ok(()), c)) => (c.conversation_id, None),
                    Ok((Err(e), c)) => (c.conversation_id, Some(format!("{}: {e:#}", c.username))),
                    Err(join) => {
                        let conn = state.db();
                        let _ = crate::log::record_throttled(
                            &conn,
                            None,
                            "summary_error",
                            &format!("summary task panicked: {join}"),
                            now,
                            crate::log::ERROR_LOG_WINDOW_MINS,
                        );
                        continue;
                    }
                };
                match failure {
                    None => {
                        failures.remove(&id);
                    }
                    Some(detail) => {
                        note_failure(&mut failures, id, now);
                        let conn = state.db();
                        let _ = crate::log::record_throttled(
                            &conn,
                            None,
                            "summary_error",
                            &detail,
                            now,
                            crate::log::ERROR_LOG_WINDOW_MINS,
                        );
                    }
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

    fn env() -> (Mutex<Connection>, tempfile::TempDir) {
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
            "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
        );
        write("defaults/prompts/summarize.md", "summarise it");
        (Mutex::new(conn), tmp)
    }

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    fn talk(conn: &Connection, id: i64, role: &str, text: &str, ts: &str) {
        crate::talk::append_text(conn, id, role, text, at(ts)).unwrap();
        crate::talk::touch(conn, id, at(ts)).unwrap();
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
        }
    }

    fn wrote(summary: &str) -> MockLLM {
        MockLLM::scripted(vec![ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "summary_write".into(),
                args: serde_json::json!({ "summary": summary }).to_string(),
            }],
        }])
    }

    #[test]
    fn only_quiet_conversations_with_uncovered_turns_are_due() {
        let (db, tmp) = env();
        let conn = db.lock().unwrap();
        let quiet = crate::talk::create(&conn, 1, "quiet", at("2026-09-17T08:00:00Z")).unwrap();
        talk(&conn, quiet, "user", "about the essay", "2026-09-17T08:00:00Z");
        let live = crate::talk::create(&conn, 1, "live", at("2026-09-17T09:55:00Z")).unwrap();
        talk(&conn, live, "user", "still typing", "2026-09-17T09:55:00Z");
        let assistant_only =
            crate::talk::create(&conn, 1, "checkin", at("2026-09-17T08:00:00Z")).unwrap();
        talk(&conn, assistant_only, "assistant", "how did it go?", "2026-09-17T08:00:00Z");

        let cutoff = at("2026-09-17T09:00:00Z");
        let quiet_only = due(&conn, tmp.path(), cutoff).unwrap();
        assert_eq!(quiet_only.len(), 1);
        assert_eq!(quiet_only[0].conversation_id, quiet);
        assert_eq!(quiet_only[0].summary, None);
        assert_eq!(quiet_only[0].summary_through, 0);
        assert_eq!(quiet_only[0].through, 1);

        crate::talk::store_summary(&conn, quiet, "Aki asked about the essay.", 1, cutoff).unwrap();
        assert!(due(&conn, tmp.path(), cutoff).unwrap().is_empty());

        talk(&conn, quiet, "user", "one more thing", "2026-09-17T08:30:00Z");
        let again = due(&conn, tmp.path(), cutoff).unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].summary.as_deref(), Some("Aki asked about the essay."));
        assert_eq!(again[0].summary_through, 1);
        assert_eq!(again[0].through, 4);
    }

    #[test]
    fn a_test_account_is_never_summarised() {
        let (db, tmp) = env();
        let conn = db.lock().unwrap();
        crate::auth::set_category(&conn, "aki", "test").unwrap();
        let id = crate::talk::create(&conn, 1, "quiet", at("2026-09-17T08:00:00Z")).unwrap();
        talk(&conn, id, "user", "hello", "2026-09-17T08:00:00Z");
        let cutoff = at("2026-09-17T09:00:00Z");
        assert!(due(&conn, tmp.path(), cutoff).unwrap().is_empty());

        std::fs::create_dir_all(tmp.path().join("users/aki")).unwrap();
        std::fs::write(tmp.path().join("users/aki/user.toml"), "nightly_enabled = true\n")
            .unwrap();
        assert_eq!(due(&conn, tmp.path(), cutoff).unwrap().len(), 1);
    }

    #[test]
    fn a_session_stores_the_summary_it_writes() {
        let (db, tmp) = env();
        {
            let conn = db.lock().unwrap();
            let id = crate::talk::create(&conn, 1, "essay", at("2026-09-17T08:00:00Z")).unwrap();
            talk(&conn, id, "user", "the essay is due friday", "2026-09-17T08:00:00Z");
            talk(&conn, id, "assistant", "I put it in Now.", "2026-09-17T08:01:00Z");
        }
        let cutoff = at("2026-09-17T09:00:00Z");
        let candidate = {
            let conn = db.lock().unwrap();
            due(&conn, tmp.path(), cutoff).unwrap().remove(0)
        };
        let llm = wrote("Aki brought the Friday essay and it went into Now.");
        run_for_conversation(&deps(&db, &tmp, &llm), &candidate, cutoff).unwrap();

        let conn = db.lock().unwrap();
        assert_eq!(
            crate::talk::summary(&conn, candidate.conversation_id).unwrap(),
            Some(("Aki brought the Friday essay and it went into Now.".to_string(), 2))
        );
        assert!(due(&conn, tmp.path(), cutoff).unwrap().is_empty());
        let seen = llm.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].system, "summarise it", "a summary carries no standing context");
    }

    #[test]
    fn a_re_summary_opens_with_the_summary_so_far() {
        let (db, tmp) = env();
        {
            let conn = db.lock().unwrap();
            let id = crate::talk::create(&conn, 1, "essay", at("2026-09-17T08:00:00Z")).unwrap();
            talk(&conn, id, "user", "the essay is due friday", "2026-09-17T08:00:00Z");
            crate::talk::store_summary(&conn, id, "Aki brought the essay.", 1, cutoff_ts()).unwrap();
            talk(&conn, id, "user", "moved to monday", "2026-09-17T08:30:00Z");
        }
        let candidate = {
            let conn = db.lock().unwrap();
            due(&conn, tmp.path(), cutoff_ts()).unwrap().remove(0)
        };
        let llm = wrote("Aki brought the essay, now due Monday.");
        run_for_conversation(&deps(&db, &tmp, &llm), &candidate, cutoff_ts()).unwrap();
        let seen = llm.seen();
        let opening = seen[0].messages.last().unwrap();
        assert!(
            matches!(opening, crate::providers::Message::User(t) if t.contains("Summary so far: Aki brought the essay.")),
            "{opening:?}"
        );
        assert_eq!(seen[0].messages.len(), 2, "only the turns the summary misses are replayed");
    }

    fn cutoff_ts() -> jiff::Timestamp {
        at("2026-09-17T09:00:00Z")
    }

    #[test]
    fn a_session_that_writes_nothing_leaves_the_conversation_alone() {
        let (db, tmp) = env();
        {
            let conn = db.lock().unwrap();
            let id = crate::talk::create(&conn, 1, "essay", at("2026-09-17T08:00:00Z")).unwrap();
            talk(&conn, id, "user", "the essay is due friday", "2026-09-17T08:00:00Z");
        }
        let candidate = {
            let conn = db.lock().unwrap();
            due(&conn, tmp.path(), cutoff_ts()).unwrap().remove(0)
        };
        let llm = MockLLM::scripted(vec![
            ChatResponse { text: "sure".into(), tool_calls: vec![] },
            ChatResponse { text: "sure".into(), tool_calls: vec![] },
        ]);
        assert!(run_for_conversation(&deps(&db, &tmp, &llm), &candidate, cutoff_ts()).is_err());
        let conn = db.lock().unwrap();
        assert_eq!(crate::talk::summary(&conn, candidate.conversation_id).unwrap(), None);
        assert_eq!(due(&conn, tmp.path(), cutoff_ts()).unwrap().len(), 1);
    }

    #[test]
    fn three_failures_in_a_row_stand_a_conversation_down_for_an_hour() {
        let mut failures = HashMap::new();
        let now = at("2026-09-17T09:00:00Z");
        for _ in 0..2 {
            note_failure(&mut failures, 7, now);
            assert!(!backed_off(&failures, 7, now));
        }
        note_failure(&mut failures, 7, now);
        assert!(backed_off(&failures, 7, now));
        assert!(backed_off(&failures, 7, at("2026-09-17T09:59:00Z")));
        assert!(!backed_off(&failures, 7, at("2026-09-17T10:01:00Z")));
        assert!(!backed_off(&failures, 8, now));
    }
}
