use crate::channels::telegram::{Callback, Update};
use crate::talk::{TurnError, Via};
use crate::AppState;
use anyhow::Result;
use argon2::password_hash::rand_core::{OsRng, RngCore};
use rusqlite::{Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Unambiguous when read off a screen and typed into a phone: no O/0, no I/1.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 6;
pub const CODE_TTL_MINS: i64 = 10;

/// The account a chat speaks for.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub user_id: i64,
    pub username: String,
    pub chat_id: i64,
}

pub fn new_code() -> String {
    let mut bytes = [0u8; CODE_LEN];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| CODE_ALPHABET[*b as usize % CODE_ALPHABET.len()] as char).collect()
}

/// One live code per user: issuing another drops the one before it, and every
/// code that has run out goes with it.
pub fn issue_code(conn: &Connection, user_id: i64, now: jiff::Timestamp) -> Result<String> {
    conn.execute("DELETE FROM telegram_link_codes WHERE user_id = ?1", [user_id])?;
    conn.execute("DELETE FROM telegram_link_codes WHERE expires_at <= ?1", [now.to_string()])?;
    let code = new_code();
    let expires = now + jiff::Span::new().minutes(CODE_TTL_MINS);
    conn.execute(
        "INSERT INTO telegram_link_codes (code, user_id, expires_at) VALUES (?1, ?2, ?3)",
        (&code, user_id, expires.to_string()),
    )?;
    Ok(code)
}

/// Spends a code on a chat, replacing whatever that account was linked to.
/// A code that has expired, or that no one issued, links nothing.
pub fn redeem(
    conn: &Connection,
    code: &str,
    chat_id: i64,
    handle: &str,
    now: jiff::Timestamp,
) -> Result<Option<i64>> {
    let user_id: Option<i64> = conn
        .query_row(
            "SELECT user_id FROM telegram_link_codes WHERE code = ?1 AND expires_at > ?2",
            (code, now.to_string()),
            |r| r.get(0),
        )
        .optional()?;
    let Some(user_id) = user_id else { return Ok(None) };
    conn.execute("DELETE FROM telegram_link_codes WHERE code = ?1", [code])?;
    conn.execute("DELETE FROM telegram_links WHERE user_id = ?1 OR chat_id = ?2", (user_id, chat_id))?;
    conn.execute(
        "INSERT INTO telegram_links (user_id, chat_id, handle, linked_at)
         VALUES (?1, ?2, ?3, ?4)",
        (user_id, chat_id, handle, now.to_string()),
    )?;
    Ok(Some(user_id))
}

pub fn unlink(conn: &Connection, user_id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM telegram_links WHERE user_id = ?1", [user_id])? > 0)
}

pub fn link_for_chat(conn: &Connection, chat_id: i64) -> Result<Option<Link>> {
    Ok(conn
        .query_row(
            "SELECT l.user_id, u.username FROM telegram_links l
             JOIN users u ON u.id = l.user_id WHERE l.chat_id = ?1",
            [chat_id],
            |r| Ok(Link { user_id: r.get(0)?, username: r.get(1)?, chat_id }),
        )
        .optional()?)
}

pub fn chat_for_user(conn: &Connection, user_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row("SELECT chat_id FROM telegram_links WHERE user_id = ?1", [user_id], |r| r.get(0))
        .optional()?)
}

pub fn cursor(conn: &Connection) -> Result<i64> {
    Ok(conn
        .query_row("SELECT last_update_id FROM telegram_cursor WHERE id = 1", [], |r| r.get(0))
        .optional()?
        .unwrap_or(0))
}

/// The cursor only moves forward, so a batch answered out of order cannot
/// replay updates the loop has already handled.
pub fn set_cursor(conn: &Connection, last_update_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO telegram_cursor (id, last_update_id) VALUES (1, ?1)
         ON CONFLICT (id) DO UPDATE SET last_update_id = MAX(last_update_id, excluded.last_update_id)",
        [last_update_id],
    )?;
    Ok(())
}

/// The thread a reply from Telegram continues: the user's most recent one that
/// crossed Telegram since `cutoff`. Nothing that recent means a fresh thread.
pub fn thread_for(conn: &Connection, user_id: i64, cutoff: jiff::Timestamp) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM conversations
             WHERE user_id = ?1 AND telegram_at IS NOT NULL AND telegram_at >= ?2
             ORDER BY telegram_at DESC, id DESC LIMIT 1",
            (user_id, cutoff.to_string()),
            |r| r.get(0),
        )
        .optional()?)
}

/// How long the loop waits after a failed poll, and the ceiling it doubles to.
const MIN_BACKOFF_SECS: u64 = 5;
const MAX_BACKOFF_SECS: u64 = 60;
/// How often one unlinked chat is told the bot is not for it.
const NOTICE_MINS: i64 = 60;

const PRIVATE: &str = "This bot is private. Link it from Note's settings.";
const FRESH: &str = "Fresh start.";
const CAPPED: &str = "You've used today's sessions.";
const BUSY: &str = "Still on your last message.";
const UNREACHABLE: &str = "Couldn't reach Note right now.";
const GONE: &str = "That one is gone.";

/// What the loop remembers between updates: nothing a restart needs back.
#[derive(Default)]
pub struct Chats {
    told: HashMap<i64, jiff::Timestamp>,
    fresh: HashSet<i64>,
    busy: HashSet<i64>,
}

/// Long-polls Telegram for as long as the server runs. A failed poll backs off
/// rather than spinning, and the cursor is persisted after every batch, so a
/// restart resumes where the last one stopped.
pub fn spawn(state: AppState) {
    if state.telegram.is_none() {
        return;
    }
    tokio::spawn(async move {
        let mut chats = Chats::default();
        let mut backoff = MIN_BACKOFF_SECS;
        loop {
            match poll_once(&state, &mut chats).await {
                Ok(()) => backoff = MIN_BACKOFF_SECS,
                Err(e) => {
                    let now = jiff::Timestamp::now();
                    {
                        let conn = state.db();
                        let _ = crate::log::record_throttled(
                            &conn,
                            None,
                            "telegram_error",
                            &e.to_string(),
                            now,
                            crate::log::ERROR_LOG_WINDOW_MINS,
                        );
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
                }
            }
        }
    });
}

/// One batch: every text message answered, then the cursor moved past all of
/// them. An update the loop dies on is replayed, never silently dropped.
pub async fn poll_once(state: &AppState, chats: &mut Chats) -> Result<()> {
    let Some(ch) = state.telegram.clone() else { return Ok(()) };
    let offset = {
        let conn = state.db();
        match cursor(&conn)? {
            0 => 0,
            last => last + 1,
        }
    };
    let polling = ch.clone();
    let batch = tokio::task::spawn_blocking(move || polling.get_updates(offset)).await??;
    for update in &batch.messages {
        receive(state, chats, update).await;
    }
    for callback in &batch.callbacks {
        receive_callback(state, callback).await;
    }
    if let Some(last) = batch.last_update_id {
        let conn = state.db();
        set_cursor(&conn, last)?;
    }
    Ok(())
}

/// One message from one chat, answered in the chat it came from.
pub async fn receive(state: &AppState, chats: &mut Chats, update: &Update) {
    let now = jiff::Timestamp::now();
    let link = {
        let conn = state.db();
        link_for_chat(&conn, update.chat_id).unwrap_or(None)
    };
    let Some(link) = link else {
        return offer_linking(state, chats, update, now).await;
    };
    let text = update.text.trim();
    if text == "/new" {
        chats.fresh.insert(update.chat_id);
        return say(state, update.chat_id, FRESH).await;
    }
    let conversation = if chats.fresh.remove(&update.chat_id) {
        None
    } else {
        let conn = state.db();
        let cutoff = now - jiff::Span::new().minutes(state.idle_summary_min as i64);
        thread_for(&conn, link.user_id, cutoff).unwrap_or(None)
    };
    match crate::talk::run_turn(state, link.user_id, &link.username, conversation, text, Via::Telegram)
        .await
    {
        Ok(turn) => {
            chats.busy.remove(&update.chat_id);
            say(state, update.chat_id, &turn.reply).await;
        }
        Err(TurnError::DailyCap) => say(state, update.chat_id, CAPPED).await,
        Err(TurnError::Busy(_)) => {
            if chats.busy.insert(update.chat_id) {
                say(state, update.chat_id, BUSY).await;
            }
        }
        Err(TurnError::Blank) => {}
        Err(_) => say(state, update.chat_id, UNREACHABLE).await,
    }
}

/// One pressed button, applied against the account the chat speaks for. Data
/// that names nothing of theirs is answered all the same, so a message from
/// before a restart or from another account's plan cannot be pressed twice.
pub async fn receive_callback(state: &AppState, cb: &Callback) {
    let now = jiff::Timestamp::now();
    let applied = {
        let conn = state.db();
        link_for_chat(&conn, cb.chat_id).unwrap_or(None).and_then(|link| {
            let did = apply(&conn, &state.config_dir, &link, &cb.data, now)?;
            let _ = crate::log::record(
                &conn,
                Some(link.user_id),
                "telegram_action",
                &format!("{}: {did}", cb.data),
            );
            Some((link.user_id, did))
        })
    };
    let Some((user_id, did)) = applied else {
        return settle_button(state, cb, GONE, None).await;
    };
    state.hub.broadcast_changed(user_id);
    let line = format!("✓ {did}");
    settle_button(state, cb, &did, Some(line)).await;
}

/// The outcome the button's data named, or `None` when it named nothing the
/// account still holds.
fn apply(
    conn: &Connection,
    config_dir: &Path,
    link: &Link,
    data: &str,
    now: jiff::Timestamp,
) -> Option<String> {
    match data.split(':').collect::<Vec<_>>().as_slice() {
        ["ev", "done", id] => crate::plan::set_status(conn, link.user_id, id.parse().ok()?, "done")
            .ok()?
            .map(|()| "Done".to_string()),
        ["ev", "drop", id] => {
            crate::plan::set_status(conn, link.user_id, id.parse().ok()?, "dropped")
                .ok()?
                .map(|()| "Dropped".to_string())
        }
        ["ev", "snooze", id, minutes] => {
            let minutes: i64 = minutes.parse().ok()?;
            crate::plan::snooze(conn, link.user_id, id.parse().ok()?, minutes)
                .ok()?
                .map(|()| format!("Snoozed {minutes} min"))
        }
        ["block", "start", id] => start_block(conn, config_dir, link, id.parse().ok()?, now),
        ["carry", _date] => None,
        _ => None,
    }
}

/// Opens the session a block was laid for: its task, for as long as it runs.
fn start_block(
    conn: &Connection,
    config_dir: &Path,
    link: &Link,
    event_id: i64,
    now: jiff::Timestamp,
) -> Option<String> {
    let (task_id, title, start, end): (i64, String, String, String) = conn
        .query_row(
            "SELECT t.id, t.title, e.wall_time, e.end_wall_time FROM events e
             JOIN plans p ON p.id = e.plan_id
             JOIN event_tasks et ON et.event_id = e.id
             JOIN tasks t ON t.id = et.task_id
             WHERE e.id = ?1 AND p.user_id = ?2 AND e.end_wall_time IS NOT NULL",
            (event_id, link.user_id),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .ok()??;
    let span = crate::plan::parse_minutes(&end).ok()? - crate::plan::parse_minutes(&start).ok()?;
    crate::work::start(
        conn,
        config_dir,
        link.user_id,
        &link.username,
        crate::work::NewSession {
            task_id: Some(task_id),
            event_id: Some(event_id),
            title,
            planned_min: (span > 0).then_some(span),
            step_index: None,
            step_count: None,
            step_name: None,
            notes: None,
        },
        now,
    )
    .ok()?;
    Some("Session started".into())
}

/// Answers the press with a toast, takes the buttons off the message, and —
/// where something was applied — writes the outcome under what it said.
async fn settle_button(state: &AppState, cb: &Callback, toast: &str, line: Option<String>) {
    let Some(ch) = state.telegram.clone() else { return };
    let (toast, text) = (toast.to_string(), line.map(|l| format!("{}\n{l}", cb.text)));
    let (callback_id, chat_id, message_id) = (cb.callback_id.clone(), cb.chat_id, cb.message_id);
    let db = state.db.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let outcome = ch
            .answer_callback(&callback_id, &toast)
            .and_then(|()| ch.clear_keyboard(chat_id, message_id))
            .and_then(|()| match text {
                Some(text) => ch.edit_text(chat_id, message_id, &text),
                None => Ok(()),
            });
        if let Err(e) = outcome {
            let conn = crate::db_guard(&db);
            let _ = crate::log::record_throttled(
                &conn,
                None,
                "telegram_error",
                &e.to_string(),
                jiff::Timestamp::now(),
                crate::log::ERROR_LOG_WINDOW_MINS,
            );
        }
    })
    .await;
}

/// A chat Note does not know: either it carries a live code, or it is told
/// where to get one — once an hour, so a stranger cannot be answered in a loop.
async fn offer_linking(
    state: &AppState,
    chats: &mut Chats,
    update: &Update,
    now: jiff::Timestamp,
) {
    if let Some(code) = update.text.trim().strip_prefix("/start ") {
        let linked = {
            let conn = state.db();
            let code = code.trim().to_uppercase();
            redeem(&conn, &code, update.chat_id, &update.handle, now)
                .unwrap_or(None)
                .and_then(|_| link_for_chat(&conn, update.chat_id).unwrap_or(None))
        };
        if let Some(link) = linked {
            chats.told.remove(&update.chat_id);
            let name = crate::config::UserConfig::load(&state.config_dir, &link.username)
                .map(|cfg| cfg.display_name)
                .unwrap_or_else(|_| link.username.clone());
            return say(state, update.chat_id, &format!("Linked to Note as {name}")).await;
        }
    }
    let quiet = chats
        .told
        .get(&update.chat_id)
        .is_some_and(|told| *told + jiff::Span::new().minutes(NOTICE_MINS) > now);
    if quiet {
        return;
    }
    chats.told.insert(update.chat_id, now);
    say(state, update.chat_id, PRIVATE).await;
}

/// A line Note says on its own account, rather than a reply a session produced.
/// A chat that will not take it is logged, never retried.
async fn say(state: &AppState, chat_id: i64, text: &str) {
    let Some(ch) = state.telegram.clone() else { return };
    let text = text.to_string();
    let db = state.db.clone();
    let _ = tokio::task::spawn_blocking(move || {
        if let Err(e) = ch.send_message(chat_id, &text, &[]) {
            let conn = crate::db_guard(&db);
            let _ = crate::log::record_throttled(
                &conn,
                None,
                "telegram_error",
                &e.to_string(),
                jiff::Timestamp::now(),
                crate::log::ERROR_LOG_WINDOW_MINS,
            );
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Connection {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role)
             VALUES ('aki', 'x', 'member'), ('bo', 'x', 'member')",
            [],
        )
        .unwrap();
        conn
    }

    fn at(rfc: &str) -> jiff::Timestamp {
        rfc.parse().unwrap()
    }

    #[test]
    fn a_code_is_six_unambiguous_characters() {
        for _ in 0..64 {
            let code = new_code();
            assert_eq!(code.chars().count(), CODE_LEN);
            assert!(
                code.bytes().all(|b| CODE_ALPHABET.contains(&b)),
                "unexpected code: {code}"
            );
        }
    }

    #[test]
    fn issuing_again_replaces_the_live_code() {
        let conn = env();
        let now = at("2026-09-17T09:00:00Z");
        let first = issue_code(&conn, 1, now).unwrap();
        let second = issue_code(&conn, 1, now).unwrap();
        assert_ne!(first, second);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM telegram_link_codes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(redeem(&conn, &first, 42, "aki", now).unwrap(), None);
        assert_eq!(redeem(&conn, &second, 42, "aki", now).unwrap(), Some(1));
    }

    #[test]
    fn a_code_dies_after_ten_minutes() {
        let conn = env();
        let now = at("2026-09-17T09:00:00Z");
        let code = issue_code(&conn, 1, now).unwrap();
        assert_eq!(redeem(&conn, &code, 42, "aki", at("2026-09-17T09:10:01Z")).unwrap(), None);
        assert_eq!(redeem(&conn, &code, 42, "aki", at("2026-09-17T09:09:59Z")).unwrap(), Some(1));
    }

    #[test]
    fn redeeming_spends_the_code_and_moves_the_link() {
        let conn = env();
        let now = at("2026-09-17T09:00:00Z");
        let code = issue_code(&conn, 1, now).unwrap();
        assert_eq!(redeem(&conn, &code, 42, "aki_t", now).unwrap(), Some(1));
        assert_eq!(redeem(&conn, &code, 43, "aki_t", now).unwrap(), None, "a code is spent once");
        assert_eq!(chat_for_user(&conn, 1).unwrap(), Some(42));
        assert_eq!(
            link_for_chat(&conn, 42).unwrap(),
            Some(Link { user_id: 1, username: "aki".into(), chat_id: 42 })
        );

        let again = issue_code(&conn, 1, now).unwrap();
        assert_eq!(redeem(&conn, &again, 43, "aki_t", now).unwrap(), Some(1));
        assert_eq!(chat_for_user(&conn, 1).unwrap(), Some(43), "one account, one chat");
        assert_eq!(link_for_chat(&conn, 42).unwrap(), None);

        let theirs = issue_code(&conn, 2, now).unwrap();
        assert_eq!(redeem(&conn, &theirs, 43, "bo_t", now).unwrap(), Some(2));
        assert_eq!(chat_for_user(&conn, 1).unwrap(), None, "one chat, one account");

        assert!(unlink(&conn, 2).unwrap());
        assert!(!unlink(&conn, 2).unwrap());
        assert_eq!(link_for_chat(&conn, 43).unwrap(), None);
    }

    #[test]
    fn an_unknown_code_links_nothing() {
        let conn = env();
        assert_eq!(redeem(&conn, "ZZZZZZ", 42, "aki", at("2026-09-17T09:00:00Z")).unwrap(), None);
        assert_eq!(link_for_chat(&conn, 42).unwrap(), None);
    }

    #[test]
    fn the_cursor_starts_at_zero_and_only_moves_forward() {
        let conn = env();
        assert_eq!(cursor(&conn).unwrap(), 0);
        set_cursor(&conn, 17).unwrap();
        assert_eq!(cursor(&conn).unwrap(), 17);
        set_cursor(&conn, 9).unwrap();
        assert_eq!(cursor(&conn).unwrap(), 17);
        set_cursor(&conn, 18).unwrap();
        assert_eq!(cursor(&conn).unwrap(), 18);
    }

    #[test]
    fn the_newest_recent_thread_continues_and_a_stale_one_does_not() {
        let conn = env();
        let now = at("2026-09-17T09:00:00Z");
        for (title, stamp) in [
            ("old", Some("2026-09-17T07:00:00Z")),
            ("recent", Some("2026-09-17T08:50:00Z")),
            ("web only", None),
        ] {
            conn.execute(
                "INSERT INTO conversations (user_id, title, created_at, updated_at, telegram_at)
                 VALUES (1, ?1, 'c', 'c', ?2)",
                (title, stamp),
            )
            .unwrap();
        }
        let cutoff = now - jiff::Span::new().minutes(30);
        assert_eq!(thread_for(&conn, 1, cutoff).unwrap(), Some(2));
        assert_eq!(thread_for(&conn, 2, cutoff).unwrap(), None, "another account's thread");

        let cutoff = now - jiff::Span::new().minutes(5);
        assert_eq!(thread_for(&conn, 1, cutoff).unwrap(), None);
    }
}
