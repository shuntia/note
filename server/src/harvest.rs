use crate::agent::SessionDeps;
use crate::tools::SessionKind;
use crate::model_text as mt;
use crate::text::Lang;
use anyhow::Result;
use rusqlite::Connection;
use std::fmt::Write as _;

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
    l: Lang,
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
        let kind = mt::harvest_kind(l, t.checkin_date.as_deref());
        let when = t
            .updated_at
            .parse::<jiff::Timestamp>()
            .map(|ts| ts.to_zoned(tz.clone()).strftime("%H:%M").to_string())
            .unwrap_or_default();
        let body = match &t.summary {
            Some(s) if t.summary_through >= t.last_id => s.clone(),
            _ => raw_turns(conn, t.id, l)?,
        };
        let block = mt::harvest_thread(l, &t.title, &kind, &when, &body);
        if out.len() + block.len() > MAX_DIGEST_BYTES {
            break;
        }
        out.push_str(&block);
    }
    Ok(out.trim_end().to_string())
}

fn raw_turns(conn: &Connection, conversation_id: i64, l: Lang) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT role, content FROM talk_messages
         WHERE conversation_id = ?1 AND role IN ('user','assistant')
         ORDER BY id DESC LIMIT ?2",
    )?;
    let mut rows = stmt
        .query_map((conversation_id, RAW_ROWS as i64), |r| {
            let role: String = r.get(0)?;
            let content: String = r.get(1)?;
            let speaker = mt::speaker(l, role == "user");
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

/// How long a conversation's episodic entry stays live.
const EPISODIC_DAYS: i64 = 90;
/// What a search returns for an episodic entry, so the line stays readable.
const MAX_EPISODIC_SUMMARY: usize = 200;

/// A conversation the night turns into one episodic memory, mechanically: the
/// summary the idle pass already wrote is the entry.
struct Episode {
    conversation_id: i64,
    summary: String,
    body: String,
}

fn first_sentence(text: &str) -> &str {
    let flat = text.trim();
    match flat.char_indices().find(|(_, c)| matches!(c, '.' | '!' | '?' | '。')) {
        Some((i, c)) => flat[..i + c.len_utf8()].trim_end_matches(['.', '。']).trim(),
        None => flat,
    }
}

fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_EPISODIC_SUMMARY {
        return flat;
    }
    flat.chars().take(MAX_EPISODIC_SUMMARY - 1).chain(std::iter::once('…')).collect()
}

fn thread_kind(conn: &Connection, id: i64, checkin: bool, via: &str) -> &'static str {
    if checkin {
        return "check-in";
    }
    let worked: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM work_sessions WHERE conversation_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if worked > 0 {
        return "session";
    }
    if via == "matrix" {
        return "matrix";
    }
    "talk"
}

fn tasks_touched(conn: &Connection, id: i64) -> Vec<i64> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT task_id FROM work_sessions
         WHERE conversation_id = ?1 AND task_id IS NOT NULL ORDER BY task_id",
    ) else {
        return Vec::new();
    };
    stmt.query_map([id], |r| r.get(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// The night's conversations that already carry a summary and have never been
/// written down, shaped into the entries that will become memory.
fn episodes(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    now: jiff::Timestamp,
) -> Result<Vec<Episode>> {
    let since = now
        .checked_sub(jiff::Span::new().hours(WINDOW_HOURS))
        .unwrap_or(jiff::Timestamp::MIN);
    let mut stmt = conn.prepare(
        "SELECT c.id, c.title, c.summary, c.updated_at, c.checkin_date IS NOT NULL, c.via
         FROM conversations c
         WHERE c.user_id = ?1 AND c.updated_at > ?2 AND c.updated_at <= ?3
           AND c.summary IS NOT NULL AND c.summary != ''
           AND NOT EXISTS (SELECT 1 FROM memory_sources m
                           WHERE m.user_id = c.user_id
                             AND m.source_id = 'conversation:' || c.id)
         ORDER BY c.updated_at",
    )?;
    let rows: Vec<(i64, String, String, String, bool, String)> = stmt
        .query_map((user_id, since.to_string(), now.to_string()), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, title, summary, updated_at, checkin, via) in rows {
        let day = updated_at
            .parse::<jiff::Timestamp>().map_or_else(|_| updated_at.clone(), |ts| ts.to_zoned(tz.clone()).date().to_string());
        let kind = thread_kind(conn, id, checkin, &via);
        let mut body = format!("{}\n\nThread: {title} ({kind}, {day}).", summary.trim());
        let touched = tasks_touched(conn, id);
        if !touched.is_empty() {
            let ids: Vec<String> = touched.iter().map(i64::to_string).collect();
            let _ = write!(body, " Tasks touched: {}.", ids.join(", "));
        }
        out.push(Episode {
            conversation_id: id,
            summary: one_line(&format!("{day} · {title}: {}", first_sentence(&summary))),
            body,
        });
    }
    Ok(out)
}

/// Writes one episodic memory per summarised conversation of the night, with no
/// model in the loop, and answers with the summary lines so the distilling
/// session reads what was just kept. `conversation:<id>` is the provenance that
/// keeps a thread from being written down twice.
fn remember(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    tz: &jiff::tz::TimeZone,
    date: &str,
    now: jiff::Timestamp,
) -> Result<Vec<String>> {
    let episodes = {
        let conn = crate::db_guard(deps.db);
        episodes(&conn, user_id, tz, now)?
    };
    if episodes.is_empty() {
        return Ok(Vec::new());
    }
    let vectors: Vec<Option<Vec<f32>>> = match deps.embeddings {
        Some(emb) => {
            let texts: Vec<String> = episodes
                .iter()
                .map(|e| crate::memory::embed_text(&e.summary, &e.body))
                .collect();
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            emb.embed(&refs).map(|vs| vs.into_iter().map(Some).collect()).unwrap_or_default()
        }
        None => Vec::new(),
    };
    let until = now
        .to_zoned(tz.clone())
        .date()
        .checked_add(jiff::Span::new().days(EPISODIC_DAYS))
        .ok()
        .map(|d| d.to_string());
    let conn = crate::db_guard(deps.db);
    let mut lines = Vec::with_capacity(episodes.len());
    for (i, e) in episodes.iter().enumerate() {
        let id = crate::memory::add_until(&conn, deps.data_dir, username, &crate::memory::Fact { category: "episodic", summary: &e.summary, body: &e.body, until: until.as_deref() }, vectors.get(i).and_then(|v| v.as_deref()))?;
        for source in [format!("harvest:{date}"), format!("conversation:{}", e.conversation_id)] {
            conn.execute(
                "INSERT OR IGNORE INTO memory_sources (user_id, source_id, memory_id)
                 VALUES (?1, ?2, ?3)",
                (user_id, &source, &id),
            )?;
        }
        lines.push(format!("- {}", e.summary));
    }
    Ok(lines)
}

/// One night's harvest: every summarised conversation becomes an episodic
/// memory in pure code, and a single session distils the timeless facts out of
/// the day. The `harvests` row is both the record and the idempotency marker,
/// so a night that fails is not retried every sweep.
pub fn run_for_user(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    tz: &jiff::tz::TimeZone,
    date: jiff::civil::Date,
    now: jiff::Timestamp,
) -> Result<()> {
    let date = date.to_string();
    {
        let conn = crate::db_guard(deps.db);
        let done: i64 = conn.query_row(
            "SELECT COUNT(*) FROM harvests WHERE user_id = ?1 AND date = ?2",
            (user_id, &date),
            |r| r.get(0),
        )?;
        if done > 0 {
            return Ok(());
        }
    }
    let kept = match remember(deps, user_id, username, tz, &date, now) {
        Ok(lines) => lines,
        Err(e) => {
            let conn = crate::db_guard(deps.db);
            let _ = crate::log::record_throttled(
                &conn,
                Some(user_id),
                "harvest_error",
                &format!("{e:#}"),
                now,
                crate::log::ERROR_LOG_WINDOW_MINS,
            );
            Vec::new()
        }
    };
    let digest = {
        let conn = crate::db_guard(deps.db);
        let l = Lang::for_user(deps.config_dir, username);
        let mut digest = digest(&conn, user_id, tz, l, now)?;
        if !kept.is_empty() {
            let _ = write!(digest, "\n\n{}\n{}", mt::tonights_episodic(l), kept.join("\n"));
        }
        digest.trim().to_string()
    };
    let written = kept.len() as i64
        + if digest.is_empty() {
        0
    } else {
        let deps = SessionDeps {
            db: deps.db,
            config_dir: deps.config_dir,
            data_dir: deps.data_dir,
            llm: deps.llm,
            embeddings: deps.embeddings,
            search: deps.search,
            task_scope: None,
            inbox_source: None,
            memory_source: Some(format!("harvest:{date}")),
            token_id: deps.token_id,
            thread_note: None,
            share: None,
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
                    &format!("{e:#}"),
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
            search: None,
            task_scope: None,
            inbox_source: None,
            memory_source: None,
            token_id: None,
            thread_note: None,
            share: None,
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

        let out = digest(&conn, 1, &tokyo(), Lang::En, now()).unwrap();
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

        assert_eq!(digest(&conn, 1, &tokyo(), Lang::En, now()).unwrap(), "");

        for i in 0..14 {
            let id = crate::talk::create(&conn, 1, &format!("t{i}"), now()).unwrap();
            talk(&conn, id, "user", "something", &format!("2026-09-17T{i:02}:00:00Z"));
        }
        let out = digest(&conn, 1, &tokyo(), Lang::En, now()).unwrap();
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
    fn every_summarised_thread_becomes_one_episodic_entry_and_only_one() {
        let (db, tmp) = env();
        {
            let conn = db.lock().unwrap();
            let id = crate::talk::create(&conn, 1, "mira", at("2026-09-17T12:00:00Z")).unwrap();
            talk(&conn, id, "user", "mira moved in next door", "2026-09-17T12:00:00Z");
            crate::talk::store_summary(
                &conn,
                id,
                "Mira moved in next door. Aki wants to bring bread over. It went well.",
                1,
                now(),
            )
            .unwrap();
            conn.execute("UPDATE conversations SET checkin_date = '2026-09-17' WHERE id = ?1", [id])
                .unwrap();
            conn.execute(
                "INSERT INTO tasks (id, user_id, title, created_at, updated_at)
                 VALUES (4, 1, 'bake', 'x', 'x')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO work_sessions (user_id, task_id, conversation_id, title, started_at,
                                            phase_started_at)
                 VALUES (1, 4, ?1, 'bake', '2026-09-17T12:30:00Z', '2026-09-17T12:30:00Z')",
                [id],
            )
            .unwrap();
            let bare = crate::talk::create(&conn, 1, "no summary", at("2026-09-17T13:00:00Z")).unwrap();
            talk(&conn, bare, "user", "still going", "2026-09-17T13:00:00Z");
        }
        let llm = MockLLM::scripted(vec![ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "harvest_done".into(),
                args: r#"{"written":0,"note":"memory holds it"}"#.into(),
            }],
        }]);
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", &tokyo(), date(), now()).unwrap();

        let conn = db.lock().unwrap();
        let (id, summary): (String, String) = conn
            .query_row(
                "SELECT id, summary FROM memory_index WHERE user = 'aki' AND category = 'episodic'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(summary, "2026-09-17 · mira: Mira moved in next door");
        let f = crate::memory::read(tmp.path(), "aki", &id).unwrap().unwrap();
        assert!(f.body.contains("Thread: mira (check-in, 2026-09-17)."), "{}", f.body);
        assert!(f.body.contains("Tasks touched: 4."), "{}", f.body);
        assert_eq!(f.until.as_deref(), Some("2026-12-17"));
        let mut sources: Vec<String> = conn
            .prepare("SELECT source_id FROM memory_sources WHERE user_id = 1")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        sources.sort();
        assert_eq!(sources, ["conversation:1", "harvest:2026-09-18"]);
        let written: i64 = conn
            .query_row("SELECT facts_written FROM harvests WHERE user_id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(written, 1, "the entry written without a model still counts");
        let opening = llm.seen().last().unwrap().messages.last().unwrap().clone();
        let crate::providers::Message::User(text) = opening else { panic!("a user opening") };
        assert!(text.contains("## Tonight's episodic entries"), "{text}");
        assert!(text.contains("2026-09-17 · mira: Mira moved in next door"), "{text}");
        assert!(text.contains("## no summary"), "an unsummarised thread is still read: {text}");
        drop(conn);

        db.lock()
            .unwrap()
            .execute("DELETE FROM harvests WHERE user_id = 1", [])
            .unwrap();
        run_for_user(&deps(&db, &tmp, &llm), 1, "aki", &tokyo(), date(), now()).unwrap();
        let n: i64 = db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM memory_index WHERE user = 'aki' AND category = 'episodic'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "a thread already written down is never written twice");
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
