use crate::channels::matrix::{Kind, Message, Reaction, SYNC_SECS};
use crate::talk::{TurnError, Via};
use crate::AppState;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashSet;

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
    busy: HashSet<i64>,
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
    let now = jiff::Timestamp::now();
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
    let conversation = if rooms.fresh.remove(&user_id) {
        None
    } else {
        let conn = state.db();
        let cutoff = now - jiff::Span::new().minutes(i64::from(state.idle_summary_min));
        thread_for(&conn, user_id, cutoff).unwrap_or(None)
    };
    match crate::talk::run_turn(state, user_id, &username, conversation, text, Via::Matrix, None).await {
        Ok(turn) => {
            rooms.busy.remove(&user_id);
            say(state, &message.room_id, user_id, &turn.reply).await;
        }
        Err(TurnError::DailyCap) => say(state, &message.room_id, user_id, &crate::text::matrix_capped(lang)).await,
        Err(TurnError::Busy(_)) => {
            if rooms.busy.insert(user_id) {
                say(state, &message.room_id, user_id, &crate::text::matrix_busy(lang)).await;
            }
        }
        Err(TurnError::Blank) => {}
        Err(TurnError::Unavailable(failure)) => {
            say(state, &message.room_id, user_id, &crate::text::failure_reason(lang, failure.reason)).await;
        }
        Err(_) => say(state, &message.room_id, user_id, &crate::text::matrix_unreachable(lang)).await,
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
        let Ok(Some((owner, data))) = crate::actions::take(&conn, &key, now) else { return };
        if owner != user_id {
            return;
        }
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
