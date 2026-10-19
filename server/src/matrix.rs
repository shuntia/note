use crate::channels::matrix::{Kind, Message, Reaction, SYNC_SECS};
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

/// Messages past this many waiting are refused, not queued.
const MAX_QUEUED: usize = 10;

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
        for reaction in &batch.reactions {
            pressed(state, reaction).await;
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
    if queue.waiting.load(Ordering::SeqCst) >= MAX_QUEUED {
        return say(state, &message.room_id, user_id, &crate::text::matrix_queue_full(lang)).await;
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

/// A reaction from the linked account in its DM presses the button it names,
/// and a short notice says what was done.
pub async fn pressed(state: &AppState, reaction: &Reaction) {
    let now = jiff::Timestamp::now();
    let done = {
        let conn = state.db();
        let Some((user_id, username)) = linked_account(&conn, &reaction.room_id, &reaction.sender) else { return };
        let key = crate::channels::matrix::reaction_key(&reaction.event_id, &reaction.key);
        let Ok(Some((_, data))) = crate::actions::take(&conn, &key, Some(user_id), now) else { return };
        let done = crate::actions::perform(&conn, &state.config_dir, user_id, &username, &data, now);
        (user_id, username, done)
    };
    let (user_id, username, done) = done;
    let lang = crate::text::Lang::for_user(&state.config_dir, &username);
    let line = match done {
        Ok(done) => {
            state.hub.broadcast_changed(user_id);
            crate::text::action_receipt(lang, done)
        }
        Err(crate::actions::Refused::Settled) => crate::text::action_settled(lang),
        Err(_) => crate::text::action_failed(lang),
    };
    say_as(state, &reaction.room_id, user_id, &line, Kind::Notice).await;
}

/// The user a Matrix sender in a room is linked as, with their username.
fn linked_account(conn: &Connection, room_id: &str, sender: &str) -> Option<(i64, String)> {
    let user_id = crate::voice::links::linked_user(conn, room_id, sender).ok().flatten()?;
    conn.query_row("SELECT username FROM users WHERE id = ?1", [user_id], |r| r.get::<_, String>(0))
        .ok()
        .map(|username| (user_id, username))
}

/// A send that fails is logged, never retried.
async fn say(state: &AppState, room_id: &str, user_id: i64, text: &str) {
    say_as(state, room_id, user_id, text, Kind::Text).await;
}

async fn say_as(state: &AppState, room_id: &str, user_id: i64, text: &str, kind: Kind) {
    let Some(ch) = state.matrix.clone() else { return };
    let (room_id, text, db) = (room_id.to_string(), text.to_string(), state.db.clone());
    let _ = tokio::task::spawn_blocking(move || {
        if let Err(e) = ch.send(&room_id, &text, kind) {
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

    #[tokio::test]
    async fn a_reaction_from_the_linked_account_presses_its_button_once() {
        let conn = env();
        conn.execute(
            "INSERT INTO matrix_links (user_id, mxid, room_id, state, created_at) VALUES (1, '@aki:t', '!dm:t', 'linked', 'x')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-10-18', 'x')", []).unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, status) VALUES (1, 'checkin', '09:00', '09:00', 'fired')",
            [],
        )
        .unwrap();
        let key = crate::channels::matrix::reaction_key("$m", "✅");
        let done = crate::channels::Action { label: "Done".into(), data: "ev:done:1".into() };
        crate::actions::keep(&conn, 1, "$m", &[key], &[done], jiff::Timestamp::now()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let state = AppState::new(conn, tmp.path().into(), tmp.path().into());
        let reaction = |sender: &str| Reaction {
            room_id: "!dm:t".into(),
            sender: sender.into(),
            event_id: "$m".into(),
            key: "✅\u{fe0f}".into(),
        };
        let status = |state: &AppState| -> String {
            state.db().query_row("SELECT status FROM events WHERE id = 1", [], |r| r.get(0)).unwrap()
        };

        pressed(&state, &reaction("@bo:t")).await;
        assert_eq!(status(&state), "fired", "a stranger in the room presses nothing");
        pressed(&state, &reaction("@aki:t")).await;
        assert_eq!(status(&state), "done");
        let left: i64 = state.db().query_row("SELECT COUNT(*) FROM message_actions", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
    }
}
