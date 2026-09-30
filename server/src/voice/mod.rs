pub mod links;
pub mod outbox;

use crate::channels::{Channel, OutboundMessage};
use note_voice_proto::{
    BoxFuture, CallBody, Dir, Handler, Outcome, Peer, PeerConfig, Refusal, RefusalCode, Reply,
    Request, Role,
};
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

pub const RING_SECS: u32 = 30;
pub const RING_BY_SECS: i64 = 10;
pub const STALE_START_SECS: i64 = 20;

type Ladder = Arc<OnceLock<Vec<Arc<dyn Channel>>>>;

pub struct Voice {
    db: Arc<Mutex<Connection>>,
    peer: Peer,
    handler: Arc<NoteHandler>,
    fallback: Ladder,
}

impl Voice {
    pub fn new(db: Arc<Mutex<Connection>>) -> Arc<Voice> {
        Self::with_config(db, PeerConfig::new(Role::Note))
    }

    pub fn with_config(db: Arc<Mutex<Connection>>, cfg: PeerConfig) -> Arc<Voice> {
        let fallback: Ladder = Arc::new(OnceLock::new());
        let handler = Arc::new(NoteHandler { db: db.clone(), fallback: fallback.clone() });
        let outbox = Box::new(outbox::SqliteOutbox::new(db.clone()));
        let peer = Peer::new(cfg, Dir::ToVoice, handler.clone(), outbox);
        Arc::new(Voice { db, peer, handler, fallback })
    }

    /// The channels a message falls through to after a ring; the voice
    /// channel itself is never among them.
    pub fn set_fallback(&self, ladder: Vec<Arc<dyn Channel>>) {
        let _ = self.fallback.set(ladder);
    }

    pub fn is_up(&self) -> bool {
        self.peer.is_up()
    }

    pub fn listen(self: &Arc<Self>, path: &Path) -> std::io::Result<tokio::task::JoinHandle<()>> {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let listener = tokio::net::UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
        Ok(tokio::spawn(note_voice_proto::listen_forever(self.peer.clone(), listener)))
    }

    pub fn spawn_sweeper(self: &Arc<Self>) {
        let voice = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tick.tick().await;
                let v = voice.clone();
                let _ = tokio::task::spawn_blocking(move || v.sweep(jiff::Timestamp::now())).await;
            }
        });
    }

    pub async fn open_dm(&self, link_id: i64, mxid: &str) -> Result<String, Refusal> {
        match self.peer.request(Request::OpenDm { link_id, mxid: mxid.to_string() }).await? {
            Reply::Dm { room_id } => Ok(room_id),
            other => Err(Refusal::new(RefusalCode::Failed, format!("unexpected reply {other:?}"))),
        }
    }

    /// Records the call, then hands the voice side a `Start` it will refuse
    /// once `RING_BY_SECS` have passed.
    pub fn start_call(
        &self,
        user_id: i64,
        link: &links::Link,
        msg: &OutboundMessage,
        now: jiff::Timestamp,
    ) -> anyhow::Result<String> {
        let room_id = link.room_id.clone().ok_or_else(|| anyhow::anyhow!("the link has no room"))?;
        let id = uuid::Uuid::new_v4().to_string();
        let ring_by = now + jiff::SignedDuration::from_secs(RING_BY_SECS);
        {
            let conn = crate::db_guard(&self.db);
            conn.execute(
                "INSERT INTO voice_calls (id, user_id, direction, message, state, ring_by, created_at)
                 VALUES (?1, ?2, 'outbound', ?3, 'starting', ?4, ?5)",
                (&id, user_id, serde_json::to_string(msg)?, ring_by.to_string(), now.to_string()),
            )?;
        }
        self.peer.send_call(
            &id,
            CallBody::Start {
                user_id,
                room_id,
                mxid: link.mxid.clone(),
                title: msg.title.clone(),
                ring_secs: RING_SECS,
                ring_by_ms: ring_by.as_millisecond(),
            },
        )?;
        Ok(id)
    }

    /// Fails every call the voice side never took up and returns how many.
    pub fn sweep(&self, now: jiff::Timestamp) -> usize {
        let cutoff = now - jiff::SignedDuration::from_secs(STALE_START_SECS);
        let stale: Vec<String> = {
            let conn = crate::db_guard(&self.db);
            let mut stmt = match conn.prepare("SELECT id, ring_by FROM voice_calls WHERE state = 'starting'") {
                Ok(s) => s,
                Err(_) => return 0,
            };
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)));
            let Ok(rows) = rows else { return 0 };
            rows.flatten()
                .filter(|(_, ring_by)| ring_by.parse::<jiff::Timestamp>().is_ok_and(|t| t < cutoff))
                .map(|(id, _)| id)
                .collect()
        };
        let mut failed = 0;
        for id in stale {
            let ended = {
                let conn = crate::db_guard(&self.db);
                end_call(&conn, &id, "failed", now).ok().flatten()
            };
            if let Some((user_id, message)) = ended {
                failed += 1;
                self.handler.fall_through(user_id, message);
            }
        }
        failed
    }
}

/// Moves the call to `ended` unless it already is, returning its user and
/// stored message only for the move that did it.
fn end_call(
    conn: &Connection,
    call_id: &str,
    outcome: &str,
    now: jiff::Timestamp,
) -> rusqlite::Result<Option<(i64, Option<String>)>> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "UPDATE voice_calls SET state = 'ended', outcome = ?2, ended_at = ?3
         WHERE id = ?1 AND state != 'ended'
         RETURNING user_id, message",
        (call_id, outcome, now.to_string()),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

pub(crate) struct NoteHandler {
    db: Arc<Mutex<Connection>>,
    fallback: Ladder,
}

impl NoteHandler {
    /// A call does not carry the message's content yet, so every ring is
    /// followed by the message through the rest of the ladder.
    fn fall_through(&self, user_id: i64, message: Option<String>) {
        let Some(raw) = message else { return };
        let Ok(msg) = serde_json::from_str::<OutboundMessage>(&raw) else {
            let conn = crate::db_guard(&self.db);
            let _ = crate::log::record(&conn, Some(user_id), "voice_error", "a stored call message did not parse");
            return;
        };
        let db = self.db.clone();
        let ladder = self.fallback.get().cloned().unwrap_or_default();
        let deliver = move || {
            let username: Option<String> = crate::db_guard(&db)
                .query_row("SELECT username FROM users WHERE id = ?1", [user_id], |r| r.get(0))
                .ok();
            if let Some(username) = username {
                crate::channels::deliver_via(&db, &ladder, user_id, &username, &msg);
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn_blocking(deliver);
            }
            Err(_) => {
                std::thread::spawn(deliver);
            }
        }
    }
}

impl Handler for NoteHandler {
    fn applied(&self, call_id: &str) -> u64 {
        crate::db_guard(&self.db)
            .query_row("SELECT applied_seq FROM voice_calls WHERE id = ?1", [call_id], |r| r.get::<_, i64>(0))
            .map(|n| n as u64)
            .unwrap_or(0)
    }

    fn apply(&self, call_id: &str, seq: u64, body: CallBody) -> Result<(), String> {
        let now = jiff::Timestamp::now();
        let ended = {
            let conn = crate::db_guard(&self.db);
            let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
            let known = tx
                .execute("UPDATE voice_calls SET applied_seq = ?2 WHERE id = ?1", (call_id, seq as i64))
                .map_err(|e| e.to_string())?
                > 0;
            let mut ended = None;
            if known {
                match &body {
                    CallBody::Ringing => {
                        tx.execute(
                            "UPDATE voice_calls SET state = 'ringing' WHERE id = ?1 AND state = 'starting'",
                            [call_id],
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    CallBody::Outcome { outcome } => {
                        ended = end_call(&tx, call_id, outcome.as_str(), now).map_err(|e| e.to_string())?;
                        if let (Some((user_id, _)), Outcome::Failed { reason }) = (&ended, outcome) {
                            let _ = crate::log::record(&tx, Some(*user_id), "voice_call_failed", reason);
                        }
                    }
                    CallBody::Ended => {
                        ended = end_call(&tx, call_id, "failed", now).map_err(|e| e.to_string())?;
                        tx.execute("DELETE FROM voice_frames WHERE call_id = ?1", [call_id])
                            .map_err(|e| e.to_string())?;
                    }
                    CallBody::Start { .. } | CallBody::HangUp => {}
                }
            }
            tx.commit().map_err(|e| e.to_string())?;
            ended
        };
        if let Some((user_id, message)) = ended {
            self.fall_through(user_id, message);
        }
        Ok(())
    }

    fn request(&self, body: Request) -> BoxFuture<Result<Reply, Refusal>> {
        let db = self.db.clone();
        Box::pin(async move {
            match body {
                Request::DmJoined { link_id, room_id } => {
                    let conn = crate::db_guard(&db);
                    links::mark_joined(&conn, link_id, &room_id, jiff::Timestamp::now())
                        .map(|_| Reply::Done)
                        .map_err(|e| Refusal::new(RefusalCode::Failed, e.to_string()))
                }
                Request::OpenDm { .. } => {
                    Err(Refusal::new(RefusalCode::BadRequest, "Note does not open rooms"))
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::mock::MockChannel;
    use crate::channels::{OutboundMessage, Urgency};
    use note_voice_proto::{CallBody, Handler, Outcome};

    fn msg() -> OutboundMessage {
        OutboundMessage {
            title: "Check-in".into(),
            body: "how is the essay going?".into(),
            urgency: Urgency::High,
            event_id: Some(4),
            conversation_id: Some(9),
            actions: Vec::new(),
        }
    }

    fn rig() -> (Arc<Voice>, Arc<MockChannel>) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", [])
            .unwrap();
        let voice = Voice::new(Arc::new(Mutex::new(conn)));
        let mock = Arc::new(MockChannel::new("push"));
        voice.set_fallback(vec![mock.clone()]);
        (voice, mock)
    }

    fn link() -> links::Link {
        links::Link { id: 1, mxid: "@aki:t".into(), room_id: Some("!r:t".into()), state: "linked".into() }
    }

    async fn settle() {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_outcome_ends_the_call_and_falls_through_once() {
        let (voice, mock) = rig();
        let id = voice.start_call(1, &link(), &msg(), jiff::Timestamp::now()).unwrap();
        let h = voice.handler.clone();
        h.apply(&id, 1, CallBody::Ringing).unwrap();
        h.apply(&id, 2, CallBody::Outcome { outcome: Outcome::Missed }).unwrap();
        h.apply(&id, 3, CallBody::Ended).unwrap();
        settle().await;
        let seen = mock.seen();
        assert_eq!(seen.len(), 1, "one fallthrough");
        assert_eq!(seen[0].1.body, "how is the essay going?");
        assert_eq!(h.applied(&id), 3);
        let conn = crate::db_guard(&voice.db);
        let (state, outcome, frames): (String, String, i64) = conn
            .query_row(
                "SELECT state, outcome, (SELECT COUNT(*) FROM voice_frames WHERE call_id = ?1)
                 FROM voice_calls WHERE id = ?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((state.as_str(), outcome.as_str(), frames), ("ended", "missed", 0));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_start_nobody_took_is_failed_by_the_sweep_once() {
        let (voice, mock) = rig();
        let then = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(60);
        let id = voice.start_call(1, &link(), &msg(), then).unwrap();
        assert_eq!(voice.sweep(jiff::Timestamp::now()), 1);
        assert_eq!(voice.sweep(jiff::Timestamp::now()), 0);
        voice.handler.apply(&id, 1, CallBody::Outcome { outcome: Outcome::Failed { reason: "late".into() } }).unwrap();
        settle().await;
        assert_eq!(mock.seen().len(), 1, "the late outcome does not deliver again");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_frame_for_an_unknown_call_is_ignored() {
        let (voice, mock) = rig();
        voice.handler.apply("ghost", 1, CallBody::Ended).unwrap();
        settle().await;
        assert!(mock.seen().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dm_joined_links_the_account() {
        let (voice, _mock) = rig();
        let link_id = {
            let conn = crate::db_guard(&voice.db);
            links::begin(&conn, 1, "@aki:t", jiff::Timestamp::now()).unwrap()
        };
        let got = voice
            .handler
            .request(note_voice_proto::Request::DmJoined { link_id, room_id: "!r:t".into() })
            .await;
        assert_eq!(got, Ok(note_voice_proto::Reply::Done));
        let conn = crate::db_guard(&voice.db);
        assert!(links::ringable(&conn, 1).unwrap().is_some());
    }
}
