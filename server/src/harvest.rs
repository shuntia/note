use crate::agent::SessionDeps;
use crate::tools::SessionKind;
use anyhow::Result;
use rusqlite::Connection;

/// What one night's reading is allowed to cost: the newest conversations, up
/// to a digest the model can hold in one prompt.
const MAX_CONVERSATIONS: usize = 12;
const MAX_DIGEST_BYTES: usize = 24 * 1024;
const RAW_ROWS: usize = 20;
const MAX_ROW_CHARS: usize = 600;
/// The stretch of conversation one run reads: the local day ending at it.
const WINDOW_HOURS: i64 = 24;

struct Thread {
    id: i64,
    title: String,
    checkin_date: Option<String>,
    updated_at: String,
    summary: Option<String>,
    summary_through: i64,
    last_id: i64,
}

/// The day's conversations as the harvest reads them: newest first, each under
/// its own heading, the summary when one covers the whole thread and the last
/// turns verbatim when none does. Empty when the user said nothing all day.
pub fn digest(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    now: jiff::Timestamp,
) -> Result<String> {
    let since = now
        .checked_sub(jiff::Span::new().hours(WINDOW_HOURS))
        .unwrap_or(jiff::Timestamp::MIN);
    let mut stmt = conn.prepare(
        "SELECT c.id, c.title, c.checkin_date, c.updated_at, c.summary,
                COALESCE(c.summary_through, 0),
                (SELECT MAX(m.id) FROM talk_messages m WHERE m.conversation_id = c.id)
         FROM conversations c
         WHERE c.user_id = ?1 AND c.updated_at > ?2 AND c.updated_at <= ?3
           AND EXISTS (SELECT 1 FROM talk_messages m
                       WHERE m.conversation_id = c.id AND m.role = 'user')
         ORDER BY c.updated_at DESC LIMIT ?4",
    )?;
    let threads = stmt
        .query_map(
            (user_id, since.to_string(), now.to_string(), MAX_CONVERSATIONS as i64),
            |r| {
                Ok(Thread {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    checkin_date: r.get(2)?,
                    updated_at: r.get(3)?,
                    summary: r.get(4)?,
                    summary_through: r.get(5)?,
                    last_id: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out = String::new();
    for t in threads {
        let kind = match &t.checkin_date {
            Some(d) => format!("checkin {d}"),
            None => "talk".to_string(),
        };
        let when = t
            .updated_at
            .parse::<jiff::Timestamp>()
            .map(|ts| ts.to_zoned(tz.clone()).strftime("%H:%M").to_string())
            .unwrap_or_default();
        let body = match &t.summary {
            Some(s) if t.summary_through >= t.last_id => s.clone(),
            _ => raw_turns(conn, t.id)?,
        };
        let block = format!("## {} ({kind}, last active {when})\n{body}\n\n", t.title);
        if out.len() + block.len() > MAX_DIGEST_BYTES {
            break;
        }
        out.push_str(&block);
    }
    Ok(out.trim_end().to_string())
}

fn raw_turns(conn: &Connection, conversation_id: i64) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT role, content FROM talk_messages
         WHERE conversation_id = ?1 AND role IN ('user','assistant')
         ORDER BY id DESC LIMIT ?2",
    )?;
    let mut rows = stmt
        .query_map((conversation_id, RAW_ROWS as i64), |r| {
            let role: String = r.get(0)?;
            let content: String = r.get(1)?;
            let speaker = if role == "user" { "user" } else { "note" };
            Ok(format!("{speaker}: {}", clip(&content)))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.reverse();
    Ok(rows.join("\n"))
}

fn clip(text: &str) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= MAX_ROW_CHARS {
        return flat;
    }
    flat.chars().take(MAX_ROW_CHARS - 1).chain(std::iter::once('…')).collect()
}

/// One night's harvest: the day's conversations read once, the facts worth
/// keeping written to memory. The `harvests` row is both the record and the
/// idempotency marker, so a night that fails is not retried every sweep.
pub fn run_for_user(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    tz: &jiff::tz::TimeZone,
    date: jiff::civil::Date,
    now: jiff::Timestamp,
) -> Result<()> {
    let date = date.to_string();
    let digest = {
        let conn = crate::db_guard(deps.db);
        let done: i64 = conn.query_row(
            "SELECT COUNT(*) FROM harvests WHERE user_id = ?1 AND date = ?2",
            (user_id, &date),
            |r| r.get(0),
        )?;
        if done > 0 {
            return Ok(());
        }
        digest(&conn, user_id, tz, now)?
    };
    let written = if digest.is_empty() {
        0
    } else {
        let deps = SessionDeps {
            db: deps.db,
            config_dir: deps.config_dir,
            data_dir: deps.data_dir,
            llm: deps.llm,
            embeddings: deps.embeddings,
            task_scope: None,
            inbox_source: None,
            memory_source: Some(format!("harvest:{date}")),
            token_id: deps.token_id,
            thread_note: None,
        };
        match crate::agent::run_session(
            &deps,
            user_id,
            username,
            SessionKind::Harvest,
            now,
            &[],
            &digest,
        ) {
            Ok(out) => {
                out.steps.iter().filter(|s| s.name == "memory_write" && !s.is_error).count() as i64
            }
            Err(e) => {
                let conn = crate::db_guard(deps.db);
                let _ = crate::log::record_throttled(
                    &conn,
                    Some(user_id),
                    "harvest_error",
                    &e.to_string(),
                    now,
                    crate::log::ERROR_LOG_WINDOW_MINS,
                );
                0
            }
        }
    };
    let conn = crate::db_guard(deps.db);
    conn.execute(
        "INSERT OR IGNORE INTO harvests (user_id, date, facts_written, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        (user_id, &date, written, now.to_string()),
    )?;
    Ok(())
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
            "display_name = \"X\"\ntimezone = \"Asia/Tokyo\"\ntemplate = \"default\"\n",
        );
        write("defaults/prompts/harvest.md", "harvest the day");
        (Mutex::new(conn), tmp)
    }

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    fn tokyo() -> jiff::tz::TimeZone {
        jiff::tz::TimeZone::get("Asia/Tokyo").unwrap()
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
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            token_id: None,
            thread_note: None,
        }
    }

    fn date() -> jiff::civil::Date {
        "2026-09-18".parse().unwrap()
    }

    fn now() -> jiff::Timestamp {
        at("2026-09-17T18:00:00Z")
    }

    #[test]
    fn the_digest_takes_the_summary_when_it_covers_the_thread_and_the_turns_when_it_does_not() {
        let (db, tmp) = env();
        let conn = db.lock().unwrap();
        let covered = crate::talk::create(&conn, 1, "the essay", at("2026-09-17T09:00:00Z")).unwrap();
        talk(&conn, covered, "user", "the essay is due friday", "2026-09-17T09:00:00Z");
        talk(&conn, covered, "assistant", "I put it in Now.", "2026-09-17T09:01:00Z");
        crate::talk::store_summary(&conn, covered, "Aki brought the Friday essay.", 2, now())
            .unwrap();
        let open = crate::talk::create(&conn, 1, "mira", at("2026-09-17T12:00:00Z")).unwrap();
        conn.execute("UPDATE conversations SET checkin_date = '2026-09-17' WHERE id = ?1", [open])
            .unwrap();
        talk(&conn, open, "user", "mira is coming over", "2026-09-17T12:00:00Z");
        crate::talk::store_summary(&conn, open, "stale", 1, now()).unwrap();
        talk(&conn, open, "assistant", "nice", "2026-09-17T12:01:00Z");

        let out = digest(&conn, 1, &tokyo(), now()).unwrap();
        assert!(out.starts_with("## mira (checkin 2026-09-17, last active 21:01)\n"), "{out}");
        assert!(out.contains("user: mira is coming over\nnote: nice"), "{out}");
        assert!(!out.contains("stale"), "a summary that misses the last turn is not used: {out}");
        assert!(out.contains("## the essay (talk, last active 18:01)\nAki brought the Friday essay."), "{out}");
        let _ = tmp;
    }

    #[test]
    fn the_digest_holds_one_day_and_only_threads_the_user_spoke_in() {
        let (db, tmp) = env();
        let conn = db.lock().unwrap();
        let old = crate::talk::create(&conn, 1, "yesterday", at("2026-09-16T09:00:00Z")).unwrap();
        talk(&conn, old, "user", "last week's essay", "2026-09-16T09:00:00Z");
        let silent = crate::talk::create(&conn, 1, "unanswered", at("2026-09-17T09:00:00Z")).unwrap();
        talk(&conn, silent, "assistant", "how did it go?", "2026-09-17T09:00:00Z");
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('bo', 'x', 'member')",
            [],
        )
        .unwrap();
        let theirs = crate::talk::create(&conn, 2, "bo's", at("2026-09-17T09:00:00Z")).unwrap();
        talk(&conn, theirs, "user", "not mine", "2026-09-17T09:00:00Z");

        assert_eq!(digest(&conn, 1, &tokyo(), now()).unwrap(), "");

        for i in 0..14 {
            let id = crate::talk::create(&conn, 1, &format!("t{i}"), now()).unwrap();
            talk(&conn, id, "user", "something", &format!("2026-09-17T{:02}:00:00Z", i));
        }
        let out = digest(&conn, 1, &tokyo(), now()).unwrap();
        assert_eq!(out.matches("## t").count(), MAX_CONVERSATIONS);
        assert!(out.len() <= MAX_DIGEST_BYTES);
        let _ = tmp;
    }

    #[test]
    fn a_harvest_runs_once_a_night_and_records_what_it_wrote() {
        let (db, tmp) = env();
        {
            let conn = db.lock().unwrap();
            let id = crate::talk::create(&conn, 1, "mira", at("2026-09-17T12:00:00Z")).unwrap();
            talk(&conn, id, "user", "mira moved in next door", "2026-09-17T12:00:00Z");
        }
        let llm = MockLLM::scripted(vec![
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "memory_write".into(),
                    args: r#"{"op":"add","category":"semantic","summary":"mira lives next door","body":"since september"}"#.into(),
                }],
            },
            ChatResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "2".into(),
                    name: "harvest_done".into(),
                    args: r#"{"written":1,"note":"the rest was today's plan"}"#.into(),
                }],
            },
        ]);
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", &tokyo(), date(), now()).unwrap();

        let conn = db.lock().unwrap();
        let written: i64 = conn
            .query_row("SELECT facts_written FROM harvests WHERE user_id=1 AND date='2026-09-18'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(written, 1);
        let source: String = conn
            .query_row("SELECT source_id FROM memory_sources WHERE user_id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(source, "harvest:2026-09-18");
        assert_eq!(crate::memory::live_count(&conn, "aki").unwrap(), 1);
        drop(conn);

        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", &tokyo(), date(), now()).unwrap();
        assert_eq!(llm.seen().len(), 2, "a second run of the same night spends nothing");
    }

    #[test]
    fn a_day_with_nothing_to_read_costs_no_session() {
        let (db, tmp) = env();
        let llm = MockLLM::scripted(vec![]);
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", &tokyo(), date(), now()).unwrap();
        assert!(llm.seen().is_empty());
        let conn = db.lock().unwrap();
        let written: i64 = conn
            .query_row("SELECT facts_written FROM harvests WHERE user_id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(written, 0);
    }

    #[test]
    fn a_failed_session_is_logged_and_still_closes_the_night() {
        struct Failing;
        impl crate::providers::LLMProvider for Failing {
            fn chat(
                &self,
                _: &crate::providers::ChatRequest,
            ) -> anyhow::Result<crate::providers::ChatResponse> {
                anyhow::bail!("down")
            }
        }
        let (db, tmp) = env();
        {
            let conn = db.lock().unwrap();
            let id = crate::talk::create(&conn, 1, "mira", at("2026-09-17T12:00:00Z")).unwrap();
            talk(&conn, id, "user", "mira moved in next door", "2026-09-17T12:00:00Z");
        }
        run_for_user(&deps(&db, &tmp, &Failing), 1, "aki", &tokyo(), date(), now()).unwrap();
        let conn = db.lock().unwrap();
        let written: i64 = conn
            .query_row("SELECT facts_written FROM harvests WHERE user_id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(written, 0);
        let logged: i64 = conn
            .query_row("SELECT COUNT(*) FROM event_log WHERE kind='harvest_error'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(logged, 1);
    }
}
