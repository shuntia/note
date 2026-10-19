use crate::channels::matrix::{Kind, Message, SYNC_SECS};
use crate::talk::{TurnError, Via};
use crate::AppState;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const MIN_BACKOFF_SECS: u64 = 5;
const MAX_BACKOFF_SECS: u64 = 60;

pub fn cursor(conn: &Connection) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT next_batch FROM matrix_cursor WHERE id = 1", [], |r| r.get(0)).optional()?)
}

pub fn set_cursor(conn: &Connection, next_batch: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO matrix_cursor (id, next_batch) VALUES (1, ?1)
         ON CONFLICT (id) DO UPDATE SET next_batch = excluded.next_batch",
        [next_batch],
    )?;
    Ok(())
}

/// The thread a message from the DM continues: the user's most recent one that
/// crossed Matrix since `cutoff`. Nothing that recent means a fresh thread.
pub fn thread_for(conn: &Connection, user_id: i64, cutoff: jiff::Timestamp) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM conversations
             WHERE user_id = ?1 AND matrix_at IS NOT NULL AND matrix_at >= ?2
             ORDER BY matrix_at DESC, id DESC LIMIT 1",
            (user_id, cutoff.to_string()),
            |r| r.get(0),
        )
        .optional()?)
}

/// What the loop remembers between messages, by user: nothing a restart needs back.
#[derive(Default)]
pub struct Rooms {
    fresh: HashSet<i64>,
    queues: HashMap<i64, Queue>,
}

/// A message that found Note busy with the same user, answered once it is free.
struct Queued {
    room_id: String,
    username: String,
    text: String,
    fresh: bool,
}

/// A user's messages waiting their turn, answered one at a time in the order
/// sent; `waiting` counts those not yet answered.
struct Queue {
    tx: tokio::sync::mpsc::UnboundedSender<Queued>,
    waiting: Arc<AtomicUsize>,
}

impl Queue {
    fn start(state: &AppState, user_id: i64) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Queued>();
        let waiting = Arc::new(AtomicUsize::new(0));
        let (state, left) = (state.clone(), waiting.clone());
        tokio::spawn(async move {
            while let Some(q) = rx.recv().await {
                let conversation = if q.fresh { None } else { recent_thread(&state, user_id) };
                let lang = crate::text::Lang::for_user(&state.config_dir, &q.username);
                let done = crate::talk::run_turn_when_free(&state, user_id, &q.username, conversation, &q.text, Via::Matrix).await;
                answer(&state, &q.room_id, user_id, lang, done).await;
                left.fetch_sub(1, Ordering::SeqCst);
            }
        });
        Self { tx, waiting }
    }
}

/// The thread a message continues now; see `thread_for`.
fn recent_thread(state: &AppState, user_id: i64) -> Option<i64> {
    let cutoff = jiff::Timestamp::now() - jiff::Span::new().minutes(i64::from(state.idle_summary_min));
    thread_for(&state.db(), user_id, cutoff).unwrap_or(None)
}

pub fn spawn(state: AppState) {
    if state.matrix.is_none() {
        return;
    }
    tokio::spawn(async move {
        let mut rooms = Rooms::default();
        let mut backoff = MIN_BACKOFF_SECS;
        loop {
            match poll_once(&state, &mut rooms).await {
                Ok(()) => backoff = MIN_BACKOFF_SECS,
                Err(e) => {
                    {
                        let conn = state.db();
                        let _ = crate::log::record_throttled(
                            &conn,
                            None,
                            "matrix_error",
                            &e.to_string(),
                            jiff::Timestamp::now(),
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

/// One sync. The first one, with no token stored, only learns where the DM
/// stands, so its history is never answered.
pub async fn poll_once(state: &AppState, rooms: &mut Rooms) -> Result<()> {
    let Some(ch) = state.matrix.clone() else { return Ok(()) };
    let since = {
        let conn = state.db();
        cursor(&conn)?
    };
    let syncing = since.clone();
    let timeout = if since.is_some() { SYNC_SECS } else { 0 };
    let batch = tokio::task::spawn_blocking(move || ch.sync(syncing.as_deref(), timeout)).await??;
    if since.is_some() {
        for message in &batch.messages {
            receive(state, rooms, message).await;
        }
    }
    let conn = state.db();
    set_cursor(&conn, &batch.next_batch)
}

/// One message from the linked account in its DM, answered there. Anything
/// else is ignored.
pub async fn receive(state: &AppState, rooms: &mut Rooms, message: &Message) {
    let linked = {
        let conn = state.db();
        crate::voice::links::linked_user(&conn, &message.room_id, &message.sender)
            .ok()
            .flatten()
            .and_then(|user_id| {
                conn.query_row("SELECT username FROM users WHERE id = ?1", [user_id], |r| r.get::<_, String>(0))
                    .ok()
                    .map(|username| (user_id, username))
            })
    };
    let Some((user_id, username)) = linked else { return };
    let lang = crate::text::Lang::for_user(&state.config_dir, &username);
    let text = message.text.trim();
    if text == "/new" {
        rooms.fresh.insert(user_id);
        return say(state, &message.room_id, user_id, &crate::text::matrix_fresh(lang)).await;
    }
    let fresh = rooms.fresh.remove(&user_id);
    let queue = rooms.queues.entry(user_id).or_insert_with(|| Queue::start(state, user_id));
    if queue.waiting.load(Ordering::SeqCst) == 0 {
        let conversation = if fresh { None } else { recent_thread(state, user_id) };
        match crate::talk::run_turn(state, user_id, &username, conversation, text, Via::Matrix, None).await {
            Err(TurnError::Busy(_)) => {}
            done => return answer(state, &message.room_id, user_id, lang, done).await,
        }
    }
    queue.waiting.fetch_add(1, Ordering::SeqCst);
    let queued = Queued { room_id: message.room_id.clone(), username, text: text.to_string(), fresh };
    if queue.tx.send(queued).is_err() {
        queue.waiting.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Writes a turn's reply, or why there is none, back to the DM.
async fn answer(
    state: &AppState,
    room_id: &str,
    user_id: i64,
    lang: crate::text::Lang,
    done: Result<crate::talk::Turn, TurnError>,
) {
    match done {
        Ok(turn) => say(state, room_id, user_id, &turn.reply).await,
        Err(TurnError::DailyCap) => say(state, room_id, user_id, &crate::text::matrix_capped(lang)).await,
        Err(TurnError::Blank) => {}
        Err(TurnError::Unavailable(failure)) => {
            say(state, room_id, user_id, &crate::text::failure_reason(lang, failure.reason)).await;
        }
        Err(_) => say(state, room_id, user_id, &crate::text::matrix_unreachable(lang)).await,
    }
}

/// A send that fails is logged, never retried.
async fn say(state: &AppState, room_id: &str, user_id: i64, text: &str) {
    let Some(ch) = state.matrix.clone() else { return };
    let (room_id, text, db) = (room_id.to_string(), text.to_string(), state.db.clone());
    let _ = tokio::task::spawn_blocking(move || {
        if let Err(e) = ch.send(&room_id, &text, Kind::Text) {
            let conn = crate::db_guard(&db);
            let _ = crate::log::record(&conn, Some(user_id), "matrix_send_error", &e.to_string());
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
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member'), ('bo', 'x', 'member')",
            [],
        )
        .unwrap();
        conn
    }

    fn at(rfc: &str) -> jiff::Timestamp {
        rfc.parse().unwrap()
    }

    #[test]
    fn the_cursor_is_empty_until_set_and_then_replaced() {
        let conn = env();
        assert_eq!(cursor(&conn).unwrap(), None);
        set_cursor(&conn, "s1").unwrap();
        set_cursor(&conn, "s2").unwrap();
        assert_eq!(cursor(&conn).unwrap().as_deref(), Some("s2"));
    }

    #[test]
    fn the_newest_recent_thread_continues_and_a_stale_one_does_not() {
        let conn = env();
        let now = at("2026-10-03T09:00:00Z");
        for (title, stamp) in
            [("old", Some("2026-10-03T07:00:00Z")), ("recent", Some("2026-10-03T08:50:00Z")), ("web only", None)]
        {
            conn.execute(
                "INSERT INTO conversations (user_id, title, created_at, updated_at, matrix_at)
                 VALUES (1, ?1, 'c', 'c', ?2)",
                (title, stamp),
            )
            .unwrap();
        }
        let cutoff = now - jiff::Span::new().minutes(30);
        assert_eq!(thread_for(&conn, 1, cutoff).unwrap(), Some(2));
        assert_eq!(thread_for(&conn, 2, cutoff).unwrap(), None, "another account's thread");
        assert_eq!(thread_for(&conn, 1, now - jiff::Span::new().minutes(5)).unwrap(), None);
    }
}
