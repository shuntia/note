use crate::agent::{AgentEvent, SessionDeps, SessionStep};
use crate::providers::{ChatRequest, LLMProvider, Message};
use crate::AppState;
use crate::model_text as mt;
use crate::text::Lang;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::fmt::Write as _;

const MAX_TITLE_CHARS: usize = 60;
const MAX_TITLE_WORDS: usize = 8;
/// How much of each opening turn the titler reads.
const TITLE_EXCHANGE_CHARS: usize = 1500;
const MAX_REASONING_BYTES: usize = 32 * 1024;
pub const MAX_MESSAGE: usize = 16 * 1024;
// History windows stay user-first/assistant-last: each success appends exactly
// one user and one assistant row, and errors persist nothing.
pub const HISTORY_LIMIT: usize = 32;

pub fn create(conn: &Connection, user_id: i64, title: &str, now: jiff::Timestamp) -> Result<i64> {
    conn.execute(
        "INSERT INTO conversations (user_id, title, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?3)",
        (user_id, title, now.to_string()),
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn owned(conn: &Connection, user_id: i64, id: i64) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM conversations WHERE id = ?1 AND user_id = ?2",
        (id, user_id),
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn touch(conn: &Connection, id: i64, now: jiff::Timestamp) -> Result<()> {
    conn.execute(
        "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
        (now.to_string(), id),
    )?;
    Ok(())
}

/// Where the user last spoke to a thread from, and so where Note answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Web,
    Matrix,
    Voice,
}

impl Via {
    pub fn as_str(&self) -> &'static str {
        match self {
            Via::Web => "web",
            Via::Matrix => "matrix",
            Via::Voice => "voice",
        }
    }
}

pub fn via_of(conn: &Connection, id: i64) -> Result<Via> {
    let via: Option<String> = conn
        .query_row("SELECT via FROM conversations WHERE id = ?1", [id], |r| r.get(0))
        .optional()?;
    Ok(match via.as_deref() {
        Some("matrix") => Via::Matrix,
        Some("voice") => Via::Voice,
        _ => Via::Web,
    })
}

/// A turn from Matrix also stamps the thread, so the next message from the DM
/// lands back in it.
pub fn mark_via(conn: &Connection, id: i64, via: Via, now: jiff::Timestamp) -> Result<()> {
    conn.execute("UPDATE conversations SET via = ?1 WHERE id = ?2", (via.as_str(), id))?;
    if via == Via::Matrix {
        stamp_matrix(conn, id, now)?;
    }
    Ok(())
}

/// When this thread last crossed the Matrix DM, whichever way the message went.
pub fn stamp_matrix(conn: &Connection, id: i64, now: jiff::Timestamp) -> Result<()> {
    conn.execute("UPDATE conversations SET matrix_at = ?1 WHERE id = ?2", (now.to_string(), id))?;
    Ok(())
}

/// The thread a check-in's question lands in: one per user per plan date, so
/// every check-in of a day appends to the same conversation and the next day
/// starts a fresh one. The question is stored as an assistant row.
pub fn checkin_thread(
    conn: &Connection,
    user_id: i64,
    date: &str,
    at: &str,
    question: &str,
    lang: crate::text::Lang,
    now: jiff::Timestamp,
) -> Result<i64> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM conversations WHERE user_id = ?1 AND checkin_date = ?2
             ORDER BY id DESC LIMIT 1",
            (user_id, date),
            |r| r.get(0),
        )
        .optional()?;
    let id = if let Some(id) = existing { id } else {
        conn.execute(
            "INSERT INTO conversations (user_id, title, created_at, updated_at, checkin_date)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            (user_id, crate::text::checkin_thread_title(lang, date.parse().ok(), at), now.to_string(), date),
        )?;
        conn.last_insert_rowid()
    };
    append_text(conn, id, "assistant", question, now)?;
    touch(conn, id, now)?;
    Ok(id)
}

/// The plan date whose check-in opened the conversation; None for a thread the
/// user started.
pub fn checkin_date(conn: &Connection, id: i64) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT checkin_date FROM conversations WHERE id = ?1", [id], |r| r.get(0))
        .optional()?
        .flatten())
}

/// `role` is `"user"` or `"assistant"`; the table's CHECK rejects anything else.
pub fn append_text(
    conn: &Connection,
    conversation_id: i64,
    role: &str,
    content: &str,
    now: jiff::Timestamp,
) -> Result<()> {
    conn.execute(
        "INSERT INTO talk_messages (conversation_id, role, content, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        (conversation_id, role, content, now.to_string()),
    )?;
    Ok(())
}

/// The assistant row, carrying the final round's thinking text and how long the
/// session took. Blank reasoning is stored as NULL, so a provider that returns
/// none reads the same as a row written before the columns existed.
pub fn append_assistant(
    conn: &Connection,
    conversation_id: i64,
    content: &str,
    reasoning: &str,
    thought_ms: u64,
    now: jiff::Timestamp,
) -> Result<()> {
    let reasoning = match reasoning.trim() {
        "" => None,
        text => Some(clip(text, MAX_REASONING_BYTES)),
    };
    conn.execute(
        "INSERT INTO talk_messages
            (conversation_id, role, content, reasoning, thought_ms, created_at)
         VALUES (?1, 'assistant', ?2, ?3, ?4, ?5)",
        (conversation_id, content, reasoning, thought_ms as i64, now.to_string()),
    )?;
    Ok(())
}

/// At most `limit` bytes, cut on a char boundary and marked with an ellipsis.
fn clip(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit - '…'.len_utf8();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// `result` is the tool output as it went back to the model, and lands in
/// `content` so every row carries its displayable text in the same column.
/// `thinking` is the reasoning of the round that made this call, held by the
/// round's first call only.
#[allow(clippy::too_many_arguments)]
pub fn append_tool(
    conn: &Connection,
    conversation_id: i64,
    tool_name: &str,
    tool_args: &str,
    result: &str,
    is_error: bool,
    thinking: Option<&str>,
    now: jiff::Timestamp,
) -> Result<()> {
    let reasoning = match thinking.unwrap_or("").trim() {
        "" => None,
        text => Some(clip(text, MAX_REASONING_BYTES)),
    };
    conn.execute(
        "INSERT INTO talk_messages
            (conversation_id, role, content, tool_name, tool_args, is_error, reasoning, created_at)
         VALUES (?1, 'tool', ?2, ?3, ?4, ?5, ?6, ?7)",
        (conversation_id, result, tool_name, tool_args, is_error, reasoning, now.to_string()),
    )?;
    Ok(())
}

/// Every row of a conversation as the API presents it, oldest-first.
pub fn messages_json(conn: &Connection, conversation_id: i64) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT id, role, content, tool_name, tool_args, is_error, created_at,
                reasoning, thought_ms
         FROM talk_messages WHERE conversation_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([conversation_id], |r| {
        Ok(serde_json::json!({
            "id": r.get::<_, i64>(0)?,
            "role": r.get::<_, String>(1)?,
            "content": r.get::<_, String>(2)?,
            "tool_name": r.get::<_, Option<String>>(3)?,
            "tool_args": r.get::<_, Option<String>>(4)?,
            "is_error": r.get::<_, bool>(5)?,
            "created_at": r.get::<_, String>(6)?,
            "reasoning": r.get::<_, Option<String>>(7)?,
            "thought_ms": r.get::<_, Option<i64>>(8)?,
        }))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The last `limit` text turns, oldest-first. Tool rows are dropped because a
/// replayed transcript has no live call ids to pair its results against.
pub fn history(conn: &Connection, conversation_id: i64, limit: usize) -> Result<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT role, content FROM talk_messages
         WHERE conversation_id = ?1 AND role IN ('user','assistant')
         ORDER BY id DESC LIMIT ?2",
    )?;
    let mut msgs = stmt
        .query_map((conversation_id, limit as i64), |r| {
            let role: String = r.get(0)?;
            let content: String = r.get(1)?;
            Ok(match role.as_str() {
                "user" => Message::User(content),
                _ => Message::Assistant { text: content, tool_calls: vec![] },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    msgs.reverse();
    Ok(msgs)
}

/// `history` restricted to the rows a summary has not covered yet, so a
/// re-summary replays only what is new.
pub fn history_after(
    conn: &Connection,
    conversation_id: i64,
    after_id: i64,
    limit: usize,
) -> Result<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT role, content FROM talk_messages
         WHERE conversation_id = ?1 AND role IN ('user','assistant') AND id > ?2
         ORDER BY id DESC LIMIT ?3",
    )?;
    let mut msgs = stmt
        .query_map((conversation_id, after_id, limit as i64), |r| {
            let role: String = r.get(0)?;
            let content: String = r.get(1)?;
            Ok(match role.as_str() {
                "user" => Message::User(content),
                _ => Message::Assistant { text: content, tool_calls: vec![] },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    msgs.reverse();
    Ok(msgs)
}

/// How many turns `history` has to choose from: past `HISTORY_LIMIT` the
/// window drops the oldest ones, and only the summary still carries them.
pub fn text_turns(conn: &Connection, conversation_id: i64) -> Result<usize> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM talk_messages
         WHERE conversation_id = ?1 AND role IN ('user','assistant')",
        [conversation_id],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

/// The summary a conversation carries, with the row it covers up to.
pub fn summary(conn: &Connection, conversation_id: i64) -> Result<Option<(String, i64)>> {
    Ok(conn
        .query_row(
            "SELECT summary, COALESCE(summary_through, 0) FROM conversations WHERE id = ?1",
            [conversation_id],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?
        .and_then(|(s, through)| s.map(|s| (s, through))))
}

/// Stores a summary and the last row it covers; `summarized_at` says when the
/// pass last ran.
pub fn store_summary(
    conn: &Connection,
    conversation_id: i64,
    summary: &str,
    through: i64,
    now: jiff::Timestamp,
) -> Result<()> {
    conn.execute(
        "UPDATE conversations SET summary = ?1, summarized_at = ?2, summary_through = ?3
         WHERE id = ?4",
        (summary, now.to_string(), through, conversation_id),
    )?;
    Ok(())
}

/// A conversation title derived from its opening message: whitespace collapsed
/// to single spaces and at most `MAX_TITLE_CHARS` chars, ellipsis included.
pub fn title_from(message: &str) -> String {
    let collapsed = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_TITLE_CHARS {
        return collapsed;
    }
    let mut title: String = collapsed.chars().take(MAX_TITLE_CHARS - 1).collect();
    title.push('…');
    title
}

/// A model's answer as a title, or `None` when what came back is prose rather
/// than a name: empty, long-winded, spread over lines, or parenthetical.
pub fn normalize_title(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.contains('\n') || raw.contains('(') || raw.contains('（') {
        return None;
    }
    let mut stripped = raw;
    loop {
        let next = stripped
            .trim_end_matches(['.', '。'])
            .trim_matches(|c| matches!(c, '"' | '\'' | '\u{201c}' | '\u{201d}' | '「' | '」' | '『' | '』'))
            .trim();
        if next == stripped {
            break;
        }
        stripped = next;
    }
    let title = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    let fits = !title.is_empty()
        && title.chars().count() <= MAX_TITLE_CHARS
        && title.split_whitespace().count() <= MAX_TITLE_WORDS;
    fits.then_some(title)
}

fn clip_chars(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

pub fn title_is_draft(conn: &Connection, conversation_id: i64) -> Result<bool> {
    let kind: Option<String> = conn
        .query_row("SELECT title_kind FROM conversations WHERE id = ?1", [conversation_id], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(kind.as_deref() == Some("draft"))
}

/// The opening of a thread as the titler reads it: what the user first said and
/// the first answer it drew. `None` where the user has not spoken yet.
fn opening_exchange(conn: &Connection, conversation_id: i64, l: Lang) -> Result<Option<String>> {
    let first = |role: &str| -> rusqlite::Result<Option<String>> {
        conn.query_row(
            "SELECT content FROM talk_messages WHERE conversation_id = ?1 AND role = ?2
             ORDER BY id LIMIT 1",
            (conversation_id, role),
            |r| r.get(0),
        )
        .optional()
    };
    let Some(user) = first("user")? else { return Ok(None) };
    let mut exchange = format!("{}: {}", mt::title_user(l), clip_chars(&user, TITLE_EXCHANGE_CHARS));
    if let Some(assistant) = first("assistant")? {
        let _ = write!(exchange, "\n{}: {}", mt::title_assistant(l), clip_chars(&assistant, TITLE_EXCHANGE_CHARS));
    }
    Ok(Some(exchange))
}

/// One tool-less call that names the thread.
fn ask_for_title(llm: &dyn LLMProvider, system: &str, exchange: &str) -> Result<String> {
    let messages = [Message::User(exchange.to_string())];
    let req = ChatRequest { system, messages: &messages, tools: &[], background: true };
    let reply = llm.chat(&req)?.text;
    normalize_title(&reply)
        .ok_or_else(|| anyhow::anyhow!("not a title: {:?}", clip_chars(&reply, 80)))
}

/// Stores a generated title; false where the user renamed the thread or another
/// pass named it first.
pub fn set_title_if_draft(conn: &Connection, conversation_id: i64, title: &str) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE conversations SET title = ?1, title_kind = 'generated'
         WHERE id = ?2 AND title_kind = 'draft'",
        (title, conversation_id),
    )?;
    Ok(rows > 0)
}

/// The summary pass's title: it may better an earlier generated one, but never
/// replaces a name the user typed.
pub fn set_title_unless_renamed(
    conn: &Connection,
    conversation_id: i64,
    title: &str,
) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE conversations SET title = ?1, title_kind = 'generated'
         WHERE id = ?2 AND title_kind != 'user'",
        (title, conversation_id),
    )?;
    Ok(rows > 0)
}

/// Names a thread from its opening exchange. Blocking, and meant to run off the
/// turn that triggered it: the DB lock is taken for the reads and the write, and
/// never held across the provider call. Without a real LLM there is nothing to
/// ask, so the draft title stands.
pub fn generate_title(state: &AppState, user_id: i64, username: &str, conversation_id: i64) {
    if state.providers_info.llm.is_none() {
        return;
    }
    let lang = Lang::for_user(&state.config_dir, username);
    let exchange = {
        let conn = state.db();
        match title_is_draft(&conn, conversation_id) {
            Ok(true) => opening_exchange(&conn, conversation_id, lang),
            Ok(false) => return,
            Err(e) => Err(e),
        }
    };
    let named = match exchange {
        Ok(None) => return,
        Ok(Some(exchange)) => crate::prompts::load_in(&state.config_dir, "title", lang)
            .map(|system| crate::agent::with_language_line(system, &state.config_dir, username))
            .and_then(|system| ask_for_title(state.llm.as_ref(), &system, &exchange))
            .and_then(|title| {
                let conn = state.db();
                set_title_if_draft(&conn, conversation_id, &title)
            }),
        Err(e) => Err(e),
    };
    match named {
        Ok(true) => state.hub.broadcast_changed(user_id),
        Ok(false) => {}
        Err(e) => {
            let conn = state.db();
            let _ = crate::log::record(
                &conn,
                Some(user_id),
                "title_error",
                &format!("conversation {conversation_id}: {e:#}"),
            );
        }
    }
}

pub struct Turn {
    pub conversation_id: i64,
    pub reply: String,
    pub steps: Vec<SessionStep>,
    pub reasoning: String,
    pub thought_ms: u64,
}

#[derive(Debug)]
pub enum TurnError {
    Blank,
    NotFound,
    DailyCap,
    Busy(crate::TalkBusy),
    /// The session did not finish; nothing was persisted.
    Unavailable(crate::failure::Failure),
    Internal,
}

/// One turn of a conversation, wherever the user spoke from: the same session,
/// the same thread notes, the same rows in the same order. `conversation` is
/// `None` to open a thread, titled from the message. A session that fails
/// persists nothing, as on the web.
pub async fn run_turn(
    state: &AppState,
    user_id: i64,
    username: &str,
    conversation: Option<i64>,
    message: &str,
    via: Via,
) -> Result<Turn, TurnError> {
    let message = message.trim().to_string();
    if message.is_empty() || message.len() > MAX_MESSAGE {
        return Err(TurnError::Blank);
    }
    {
        let conn = state.db();
        let _ = crate::presence::touch(&conn, user_id, jiff::Timestamp::now());
    }
    let mut notes: Vec<String> = Vec::new();
    let lang = Lang::for_user(&state.config_dir, username);
    if let Some(id) = conversation {
        let conn = state.db();
        match owned(&conn, user_id, id) {
            Ok(true) => {}
            Ok(false) => return Err(TurnError::NotFound),
            Err(_) => return Err(TurnError::Internal),
        }
        match checkin_date(&conn, id) {
            Ok(Some(date)) => notes.push(mt::checkin_thread_note(lang, &date)),
            Ok(None) => {}
            Err(_) => return Err(TurnError::Internal),
        }
        if text_turns(&conn, id).unwrap_or(0) > HISTORY_LIMIT {
            if let Ok(Some((summary, _))) = summary(&conn, id) {
                notes.push(mt::summary_thread_note(lang, &summary));
            }
        }
    }
    let thread_note = (!notes.is_empty()).then(|| notes.join("\n\n"));
    if crate::api::daily_cap_reached(state, user_id) {
        return Err(TurnError::DailyCap);
    }
    let permit = state.talk_gate.try_enter(user_id).map_err(TurnError::Busy)?;

    let st = state.clone();
    let session_user = username.to_string();
    let said = message.clone();
    let result = tokio::task::spawn_blocking(move || {
        // held here, not in the caller's future, so a cancelled request still
        // holds the slot until the session it orphaned actually finishes
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
            thread_note,
            share: None,
        };
        let now = jiff::Timestamp::now();
        let past = match conversation {
            Some(id) => {
                let conn = st.db();
                history(&conn, id, HISTORY_LIMIT)?
            }
            None => Vec::new(),
        };
        // A brand-new conversation has no id yet, so its frames carry null
        // until the reply hands the client one.
        let seq = std::cell::Cell::new(0u64);
        let on_event = |ev: AgentEvent| {
            let n = seq.replace(seq.get() + 1);
            st.hub.send(user_id, &crate::channels::ws::agent_frame(conversation, n, &ev));
        };
        let out = crate::agent::run_session_watched(
            &deps,
            user_id,
            &session_user,
            crate::tools::SessionKind::Talk,
            now,
            &past,
            &said,
            &on_event,
        )?;
        let reply = if out.reply.trim().is_empty() {
            crate::text::empty_reply(crate::text::Lang::for_user(&st.config_dir, &session_user))
        } else {
            out.reply.clone()
        };
        let conn = st.db();
        let conv_id = match conversation {
            Some(id) => id,
            None => create(&conn, user_id, &title_from(&said), now)?,
        };
        append_text(&conn, conv_id, "user", &said, now)?;
        for s in &out.steps {
            append_tool(
                &conn,
                conv_id,
                &s.name,
                &s.args,
                &s.result,
                s.is_error,
                s.thinking.as_deref(),
                now,
            )?;
        }
        append_assistant(&conn, conv_id, &reply, &out.reasoning, out.thought_ms, now)?;
        touch(&conn, conv_id, now)?;
        mark_via(&conn, conv_id, via, now)?;
        Ok::<_, anyhow::Error>(Turn {
            conversation_id: conv_id,
            reply,
            steps: out.steps,
            reasoning: out.reasoning,
            thought_ms: out.thought_ms,
        })
    })
    .await;
    match result {
        Ok(Ok(turn)) => {
            let st = state.clone();
            let titled_user = username.to_string();
            let conv_id = turn.conversation_id;
            tokio::task::spawn_blocking(move || {
                generate_title(&st, user_id, &titled_user, conv_id);
            });
            Ok(turn)
        }
        Ok(Err(e)) => {
            let conn = state.db();
            let _ = crate::log::record(&conn, Some(user_id), "talk_error", &format!("{e:#}"));
            Err(TurnError::Unavailable(crate::failure::Failure::of(&e)))
        }
        Err(e) => {
            let conn = state.db();
            let _ =
                crate::log::record(&conn, Some(user_id), "talk_error", &format!("talk task failed: {e}"));
            Err(TurnError::Internal)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn_with_conversation() -> Connection {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        create(&conn, 1, "chat", jiff::Timestamp::now()).unwrap();
        conn
    }

    fn now() -> jiff::Timestamp {
        jiff::Timestamp::now()
    }

    #[test]
    fn a_days_checkins_share_one_thread_and_the_next_day_opens_another() {
        let conn = conn_with_conversation();
        let morning: jiff::Timestamp = "2026-09-17T00:00:00Z".parse().unwrap();
        let noon: jiff::Timestamp = "2026-09-17T03:00:00Z".parse().unwrap();
        let first =
            checkin_thread(&conn, 1, "2026-09-17", "08:00", "Morning — how did you sleep?", crate::text::Lang::En, morning)
                .unwrap();
        let again =
            checkin_thread(&conn, 1, "2026-09-17", "11:00", "Midday. How is it going?", crate::text::Lang::En, noon)
                .unwrap();
        assert_eq!(first, again);
        assert_ne!(first, 1, "the user's own thread is never reused");

        let rows = messages_json(&conn, first).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r["role"] == "assistant"));
        assert_eq!(rows[1]["content"], "Midday. How is it going?");

        let (title, updated): (String, String) = conn
            .query_row("SELECT title, updated_at FROM conversations WHERE id = ?1", [first], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(title, "Thursday's 08:00 check-in", "the opener names the day, not the question");
        assert_eq!(updated, noon.to_string());
        assert_eq!(checkin_date(&conn, first).unwrap().as_deref(), Some("2026-09-17"));
        assert_eq!(checkin_date(&conn, 1).unwrap(), None);
        assert_eq!(checkin_date(&conn, 99).unwrap(), None);

        let next = checkin_thread(&conn, 1, "2026-09-18", "08:00", "Morning again", crate::text::Lang::En, noon).unwrap();
        assert_ne!(next, first);
    }

    #[test]
    fn a_checkin_thread_belongs_to_its_user_alone() {
        let conn = conn_with_conversation();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('bo', 'x', 'member')",
            [],
        )
        .unwrap();
        let mine = checkin_thread(&conn, 1, "2026-09-17", "09:00", "hi", crate::text::Lang::En, now()).unwrap();
        let theirs = checkin_thread(&conn, 2, "2026-09-17", "09:00", "hi", crate::text::Lang::En, now()).unwrap();
        assert_ne!(mine, theirs);
        assert!(owned(&conn, 2, theirs).unwrap());
        assert!(!owned(&conn, 2, mine).unwrap());
    }

    #[test]
    fn history_after_replays_only_the_rows_a_summary_missed() {
        let conn = conn_with_conversation();
        for i in 0..4 {
            append_text(&conn, 1, "user", &format!("m{i}"), now()).unwrap();
            append_tool(&conn, 1, "task_create", "{}", "{}", false, None, now()).unwrap();
        }
        let after: i64 = conn
            .query_row("SELECT id FROM talk_messages WHERE content = 'm1'", [], |r| r.get(0))
            .unwrap();
        let msgs = history_after(&conn, 1, after, 10).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[0], Message::User(t) if t == "m2"));
        assert!(matches!(&msgs[1], Message::User(t) if t == "m3"));

        let msgs = history_after(&conn, 1, after, 1).unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(matches!(&msgs[0], Message::User(t) if t == "m3"));
        assert_eq!(history_after(&conn, 1, 0, 10).unwrap().len(), 4);
        assert_eq!(text_turns(&conn, 1).unwrap(), 4);
    }

    #[test]
    fn a_stored_summary_reads_back_with_the_row_it_covers() {
        let conn = conn_with_conversation();
        assert_eq!(summary(&conn, 1).unwrap(), None);
        append_text(&conn, 1, "user", "hi", now()).unwrap();
        let at: jiff::Timestamp = "2026-09-17T09:00:00Z".parse().unwrap();
        store_summary(&conn, 1, "Aki said hello.", 1, at).unwrap();
        assert_eq!(summary(&conn, 1).unwrap(), Some(("Aki said hello.".to_string(), 1)));
        let when: String = conn
            .query_row("SELECT summarized_at FROM conversations WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(when, at.to_string());
        assert_eq!(summary(&conn, 99).unwrap(), None);
    }

    #[test]
    fn title_from_passes_short_messages_through() {
        assert_eq!(title_from("call mom"), "call mom");
    }

    #[test]
    fn title_from_collapses_whitespace_runs() {
        assert_eq!(title_from("  call\n\n  mom   today \t"), "call mom today");
    }

    #[test]
    fn title_from_truncates_on_a_char_boundary() {
        let title = title_from(&"日".repeat(100));
        assert!(title.ends_with('…'));
        assert_eq!(title.chars().count(), MAX_TITLE_CHARS);
        assert!(title.chars().take(MAX_TITLE_CHARS - 1).all(|c| c == '日'));

        let exact = "a".repeat(MAX_TITLE_CHARS);
        assert_eq!(title_from(&exact), exact);
    }

    #[test]
    fn a_title_is_kept_only_when_it_reads_as_a_name() {
        for (raw, want) in [
            ("AP Physics test on Friday", Some("AP Physics test on Friday")),
            ("  \"Dentist on Tuesday\".  ", Some("Dentist on Tuesday")),
            ("\u{201c}Grandma's birthday\u{201d}", Some("Grandma's birthday")),
            ("Essay   plan\tfor Monday", Some("Essay plan for Monday")),
            ("金曜日の物理のテスト", Some("金曜日の物理のテスト")),
            ("「金曜の物理のテスト」。", Some("金曜の物理のテスト")),
            ("物理のテスト（金曜）", None),
            ("   ", None),
            ("Title: a thread about\nthe essay", None),
            ("A title (about the essay)", None),
            ("a b c d e f g h i", None),
            ("x the user asked what to do about the physics test that is set for Friday", None),
        ] {
            assert_eq!(normalize_title(raw).as_deref(), want, "{raw:?}");
        }
    }

    #[test]
    fn a_generated_title_replaces_a_draft_and_never_a_rename() {
        use crate::providers::{mock::MockLLM, ChatResponse};
        let conn = conn_with_conversation();
        assert!(opening_exchange(&conn, 1, Lang::En).unwrap().is_none());
        append_text(&conn, 1, "user", &"の".repeat(TITLE_EXCHANGE_CHARS + 50), now()).unwrap();
        append_text(&conn, 1, "assistant", "I put it in Now.", now()).unwrap();

        let exchange = opening_exchange(&conn, 1, Lang::En).unwrap().unwrap();
        assert_eq!(
            exchange.lines().next().unwrap().chars().count(),
            TITLE_EXCHANGE_CHARS + "User: ".len(),
            "each turn is clipped"
        );
        assert!(exchange.ends_with("Assistant: I put it in Now."));

        let llm = MockLLM::scripted(vec![
            ChatResponse { text: " \"Physics test on Friday\" ".into(), tool_calls: vec![] },
            ChatResponse { text: "Second thoughts".into(), tool_calls: vec![] },
        ]);
        assert!(title_is_draft(&conn, 1).unwrap());
        let title = ask_for_title(&llm, "name it", &exchange).unwrap();
        assert_eq!(title, "Physics test on Friday");
        assert!(set_title_if_draft(&conn, 1, &title).unwrap());
        assert!(!title_is_draft(&conn, 1).unwrap());

        let again = ask_for_title(&llm, "name it", &exchange).unwrap();
        assert!(!set_title_if_draft(&conn, 1, &again).unwrap(), "a named thread is left alone");
        assert!(set_title_unless_renamed(&conn, 1, &again).unwrap());

        conn.execute("UPDATE conversations SET title_kind = 'user' WHERE id = 1", []).unwrap();
        assert!(!set_title_unless_renamed(&conn, 1, "something else").unwrap());
        let (title, kind): (String, String) = conn
            .query_row("SELECT title, title_kind FROM conversations WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((title.as_str(), kind.as_str()), ("Second thoughts", "user"));
    }

    #[test]
    fn a_reply_that_is_not_a_title_is_an_error_rather_than_a_name() {
        use crate::providers::{mock::MockLLM, ChatResponse};
        let llm = MockLLM::scripted(vec![ChatResponse {
            text: "The user asked about their physics test, which is on Friday.".into(),
            tool_calls: vec![],
        }]);
        let err = ask_for_title(&llm, "name it", "User: hi").unwrap_err().to_string();
        assert!(err.contains("not a title"), "{err}");
    }

    #[test]
    fn the_role_check_rejects_unknown_roles() {
        let conn = conn_with_conversation();
        assert!(append_text(&conn, 1, "system", "x", now()).is_err());
    }

    #[test]
    fn history_excludes_tool_rows_and_maps_roles_oldest_first() {
        let conn = conn_with_conversation();
        append_text(&conn, 1, "user", "hi", now()).unwrap();
        append_tool(&conn, 1, "task_create", r#"{"title":"x"}"#, "{}", false, None, now()).unwrap();
        append_text(&conn, 1, "assistant", "done", now()).unwrap();

        let msgs = history(&conn, 1, 10).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[0], Message::User(t) if t == "hi"));
        assert!(
            matches!(&msgs[1], Message::Assistant { text, tool_calls } if text == "done" && tool_calls.is_empty())
        );
    }

    #[test]
    fn history_limit_takes_the_last_rows() {
        let conn = conn_with_conversation();
        for i in 0..5 {
            append_text(&conn, 1, "user", &format!("m{i}"), now()).unwrap();
        }
        let msgs = history(&conn, 1, 2).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[0], Message::User(t) if t == "m3"));
        assert!(matches!(&msgs[1], Message::User(t) if t == "m4"));
    }

    #[test]
    fn history_is_scoped_to_its_conversation() {
        let conn = conn_with_conversation();
        create(&conn, 1, "other", now()).unwrap();
        append_text(&conn, 1, "user", "mine", now()).unwrap();
        append_text(&conn, 2, "user", "theirs", now()).unwrap();
        let msgs = history(&conn, 1, 10).unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(matches!(&msgs[0], Message::User(t) if t == "mine"));
    }

    #[test]
    fn owned_is_false_for_another_user_and_for_absent_ids() {
        let conn = conn_with_conversation();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('bo', 'x', 'member')",
            [],
        )
        .unwrap();
        assert!(owned(&conn, 1, 1).unwrap());
        assert!(!owned(&conn, 2, 1).unwrap());
        assert!(!owned(&conn, 1, 99).unwrap());
    }

    #[test]
    fn touch_bumps_updated_at_without_moving_created_at() {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let start: jiff::Timestamp = "2026-08-30T09:00:00Z".parse().unwrap();
        let id = create(&conn, 1, "chat", start).unwrap();
        let later: jiff::Timestamp = "2026-08-30T10:00:00Z".parse().unwrap();
        touch(&conn, id, later).unwrap();
        let (created, updated): (String, String) = conn
            .query_row("SELECT created_at, updated_at FROM conversations WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(created, start.to_string());
        assert_eq!(updated, later.to_string());
    }

    #[test]
    fn deleting_a_conversation_cascades_to_its_messages() {
        let conn = conn_with_conversation();
        append_text(&conn, 1, "user", "hi", now()).unwrap();
        append_tool(&conn, 1, "task_create", "{}", "{}", true, None, now()).unwrap();
        conn.execute("DELETE FROM conversations WHERE id = 1", []).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM talk_messages WHERE conversation_id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn append_tool_stores_the_call_beside_its_result() {
        let conn = conn_with_conversation();
        append_tool(&conn, 1, "task_create", r#"{"title":"x"}"#, r#"{"id":1}"#, true, None, now())
            .unwrap();
        let (role, content, name, args, is_error): (String, String, String, String, bool) = conn
            .query_row(
                "SELECT role, content, tool_name, tool_args, is_error FROM talk_messages WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(role, "tool");
        assert_eq!(content, r#"{"id":1}"#);
        assert_eq!(name, "task_create");
        assert_eq!(args, r#"{"title":"x"}"#);
        assert!(is_error);
    }

    #[test]
    fn append_assistant_stores_the_trace_and_messages_json_returns_it() {
        let conn = conn_with_conversation();
        append_text(&conn, 1, "user", "hi", now()).unwrap();
        append_tool(&conn, 1, "task_create", "{}", "{}", false, Some("  a task  "), now()).unwrap();
        append_assistant(&conn, 1, "done", "  first\n\nsecond  ", 2400, now()).unwrap();

        let rows = messages_json(&conn, 1).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows[0]["reasoning"].is_null() && rows[0]["thought_ms"].is_null());
        assert_eq!(rows[1]["reasoning"], "a task");
        assert!(rows[1]["thought_ms"].is_null());
        assert_eq!(rows[2]["role"], "assistant");
        assert_eq!(rows[2]["reasoning"], "first\n\nsecond");
        assert_eq!(rows[2]["thought_ms"], 2400);
    }

    #[test]
    fn a_call_no_thinking_led_to_stores_none() {
        let conn = conn_with_conversation();
        append_tool(&conn, 1, "task_create", "{}", "{}", false, Some("   "), now()).unwrap();
        append_tool(&conn, 1, "memory_query", "{}", "{}", false, None, now()).unwrap();
        let rows = messages_json(&conn, 1).unwrap();
        assert!(rows[0]["reasoning"].is_null());
        assert!(rows[1]["reasoning"].is_null());
    }

    #[test]
    fn over_long_thinking_on_a_call_is_clipped_too() {
        let conn = conn_with_conversation();
        let long = "日".repeat(MAX_REASONING_BYTES);
        append_tool(&conn, 1, "task_create", "{}", "{}", false, Some(&long), now()).unwrap();
        let stored = messages_json(&conn, 1).unwrap()[0]["reasoning"].as_str().unwrap().to_string();
        assert!(stored.len() <= MAX_REASONING_BYTES);
        assert!(stored.ends_with('…'));
    }

    #[test]
    fn blank_reasoning_is_null_and_the_timing_is_kept() {
        let conn = conn_with_conversation();
        append_assistant(&conn, 1, "done", "   \n ", 0, now()).unwrap();
        let rows = messages_json(&conn, 1).unwrap();
        assert!(rows[0]["reasoning"].is_null());
        assert_eq!(rows[0]["thought_ms"], 0);
    }

    #[test]
    fn a_row_written_before_the_columns_existed_reads_as_null() {
        let conn = conn_with_conversation();
        append_text(&conn, 1, "assistant", "old", now()).unwrap();
        let rows = messages_json(&conn, 1).unwrap();
        assert!(rows[0]["reasoning"].is_null());
        assert!(rows[0]["thought_ms"].is_null());
    }

    #[test]
    fn over_long_reasoning_is_clipped_on_a_char_boundary() {
        let conn = conn_with_conversation();
        append_assistant(&conn, 1, "done", &"日".repeat(MAX_REASONING_BYTES), 10, now()).unwrap();
        let stored = messages_json(&conn, 1).unwrap()[0]["reasoning"].as_str().unwrap().to_string();
        assert!(stored.len() <= MAX_REASONING_BYTES);
        assert!(stored.ends_with('…'));
        assert!(stored.trim_end_matches('…').chars().all(|c| c == '日'));
    }

    #[test]
    fn append_text_leaves_the_tool_columns_null() {
        let conn = conn_with_conversation();
        append_text(&conn, 1, "user", "hi", now()).unwrap();
        let (name, args): (Option<String>, Option<String>) = conn
            .query_row("SELECT tool_name, tool_args FROM talk_messages WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(name.is_none());
        assert!(args.is_none());
    }
}
