pub mod links;
pub mod outbox;

use crate::channels::{Channel, OutboundMessage};
use note_voice_proto::{
    BoxFuture, CallBody, Dir, Handler, Outcome, Peer, PeerConfig, Refusal, RefusalCode, Reply,
    Request, Role,
};
use rusqlite::Connection;
use std::path::Path;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

pub const RING_SECS: u32 = 30;
pub const RING_BY_SECS: i64 = 10;
pub const STALE_START_SECS: i64 = 20;
pub const STALE_RING_SECS: i64 = 90;

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
        let outbox = Box::new(outbox::SqliteOutbox::new(db.clone()));
        Self::with_outbox(db, cfg, outbox)
    }

    fn with_outbox(db: Arc<Mutex<Connection>>, cfg: PeerConfig, outbox: Box<dyn note_voice_proto::Outbox>) -> Arc<Voice> {
        let fallback: Ladder = Arc::new(OnceLock::new());
        let handler =
            Arc::new(NoteHandler { db: db.clone(), fallback: fallback.clone(), in_flight: Arc::default() });
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

    /// Every 5 s fails stale calls; the first tick after the fallback is set
    /// also re-delivers what an earlier run ended but never delivered.
    pub fn spawn_sweeper(self: &Arc<Self>) {
        let voice = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            let mut redriven = false;
            loop {
                tick.tick().await;
                let v = voice.clone();
                let redrive = !redriven && v.fallback.get().is_some();
                redriven |= redrive;
                let _ = tokio::task::spawn_blocking(move || {
                    if redrive {
                        v.redrive();
                    }
                    v.sweep(jiff::Timestamp::now())
                })
                .await;
            }
        });
    }

    pub async fn open_dm(&self, link_id: i64, mxid: &str) -> Result<String, Refusal> {
        match self.peer.request(Request::OpenDm { link_id, mxid: mxid.to_string() }).await? {
            Reply::Dm { room_id } => Ok(room_id),
            other @ Reply::Done => Err(Refusal::new(RefusalCode::Failed, format!("unexpected reply {other:?}"))),
        }
    }

    /// Records the call, then hands the voice side a `Start` it will refuse
    /// once `RING_BY_SECS` have passed. On `Err` the call is already ended and
    /// never falls through: the caller's ladder delivers the message.
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
            let busy: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM voice_calls WHERE user_id = ?1 AND state IN ('starting', 'ringing'))",
                [user_id],
                |r| r.get(0),
            )?;
            anyhow::ensure!(!busy, "a ring is already under way");
            conn.execute(
                "INSERT INTO voice_calls (id, user_id, direction, message, state, ring_by, created_at)
                 VALUES (?1, ?2, 'outbound', ?3, 'starting', ?4, ?5)",
                (&id, user_id, serde_json::to_string(msg)?, ring_by.to_string(), now.to_string()),
            )?;
        }
        let sent = self.peer.send_call(
            &id,
            CallBody::Start {
                user_id,
                room_id,
                mxid: link.mxid.clone(),
                title: msg.title.clone(),
                ring_secs: RING_SECS,
                ring_by_ms: ring_by.as_millisecond(),
            },
        );
        if let Err(e) = sent {
            let conn = crate::db_guard(&self.db);
            conn.execute(
                "UPDATE voice_calls SET state = 'ended', outcome = 'failed', ended_at = ?2, fell_through_at = ?2
                 WHERE id = ?1",
                (&id, now.to_string()),
            )?;
            return Err(e.into());
        }
        Ok(id)
    }

    /// Fails every call the voice side never took up or never reported the
    /// end of, and returns how many. Does nothing until the fallback is set.
    pub fn sweep(&self, now: jiff::Timestamp) -> usize {
        if self.fallback.get().is_none() {
            return 0;
        }
        let stale = self.stale_calls(now);
        self.fail_stale(stale, now)
    }

    /// Delivers every ended call's message that was never delivered after its
    /// ring, and returns how many it sent on.
    pub fn redrive(&self) -> usize {
        if self.fallback.get().is_none() {
            return 0;
        }
        let owed: Vec<(String, i64, String)> = {
            let conn = crate::db_guard(&self.db);
            let Ok(mut stmt) = conn.prepare(
                "SELECT id, user_id, message FROM voice_calls
                 WHERE state = 'ended' AND message IS NOT NULL AND fell_through_at IS NULL",
            ) else {
                return 0;
            };
            let Ok(rows) = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))) else {
                return 0;
            };
            rows.flatten().collect()
        };
        let n = owed.len();
        for (id, user_id, message) in owed {
            self.handler.fall_through(&id, user_id, Some(message));
        }
        n
    }

    /// Each stale call's id and the state it went stale in.
    fn stale_calls(&self, now: jiff::Timestamp) -> Vec<(String, String)> {
        let cutoff = |state: &str| {
            let secs = if state == "ringing" { STALE_RING_SECS } else { STALE_START_SECS };
            now - jiff::SignedDuration::from_secs(secs)
        };
        let conn = crate::db_guard(&self.db);
        let Ok(mut stmt) =
            conn.prepare("SELECT id, state, ring_by FROM voice_calls WHERE state IN ('starting', 'ringing')")
        else {
            return Vec::new();
        };
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)));
        let Ok(rows) = rows else { return Vec::new() };
        rows.flatten()
            .filter(|(_, state, ring_by)| ring_by.parse::<jiff::Timestamp>().is_ok_and(|t| t < cutoff(state)))
            .map(|(id, state, _)| (id, state))
            .collect()
    }

    /// Fails each call still in the state it went stale in, tells the voice
    /// side to hang up, and delivers the message.
    fn fail_stale(&self, stale: Vec<(String, String)>, now: jiff::Timestamp) -> usize {
        let mut failed = 0;
        for (id, state) in stale {
            let ended = {
                let conn = crate::db_guard(&self.db);
                end_call(&conn, &id, "failed", Some(&state), now).ok().flatten()
            };
            if let Some((user_id, message)) = ended {
                failed += 1;
                if let Err(e) = self.peer.send_call(&id, CallBody::HangUp) {
                    eprintln!("voice: journaling a hang-up for {id} failed: {e}");
                }
                self.handler.fall_through(&id, user_id, message);
            }
        }
        failed
    }
}

/// Moves the call to `ended` unless it already is (or, given `only_from`,
/// unless it is in that state), returning its user and stored message only
/// for the move that did it.
fn end_call(
    conn: &Connection,
    call_id: &str,
    outcome: &str,
    only_from: Option<&str>,
    now: jiff::Timestamp,
) -> rusqlite::Result<Option<(i64, Option<String>)>> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "UPDATE voice_calls SET state = 'ended', outcome = ?2, ended_at = ?3
         WHERE id = ?1 AND state != 'ended' AND (?4 IS NULL OR state = ?4)
         RETURNING user_id, message",
        (call_id, outcome, now.to_string(), only_from),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

fn claim(set: &Mutex<HashSet<String>>) -> std::sync::MutexGuard<'_, HashSet<String>> {
    set.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) struct NoteHandler {
    db: Arc<Mutex<Connection>>,
    fallback: Ladder,
    in_flight: Arc<Mutex<HashSet<String>>>,
}

impl NoteHandler {
    /// A call does not carry the message's content yet, so every ring is
    /// followed by the message through the rest of the ladder. Delivery is
    /// stamped in `fell_through_at`, and a call already stamped or already
    /// being delivered is skipped.
    fn fall_through(&self, call_id: &str, user_id: i64, message: Option<String>) {
        let Some(raw) = message else { return };
        if !claim(&self.in_flight).insert(call_id.to_string()) {
            return;
        }
        let stamped = |db: &Arc<Mutex<Connection>>, call_id: &str| {
            let _ = crate::db_guard(db).execute(
                "UPDATE voice_calls SET fell_through_at = ?2 WHERE id = ?1",
                (call_id, jiff::Timestamp::now().to_string()),
            );
        };
        let owed = crate::db_guard(&self.db)
            .query_row("SELECT fell_through_at IS NULL FROM voice_calls WHERE id = ?1", [call_id], |r| {
                r.get::<_, bool>(0)
            })
            .unwrap_or(false);
        if !owed {
            claim(&self.in_flight).remove(call_id);
            return;
        }
        let Ok(msg) = serde_json::from_str::<OutboundMessage>(&raw) else {
            {
                let conn = crate::db_guard(&self.db);
                let _ = crate::log::record(&conn, Some(user_id), "voice_error", "a stored call message did not parse");
            }
            stamped(&self.db, call_id);
            claim(&self.in_flight).remove(call_id);
            return;
        };
        let db = self.db.clone();
        let ladder = self.fallback.get().cloned().unwrap_or_default();
        let in_flight = self.in_flight.clone();
        let call_id = call_id.to_string();
        let deliver = move || {
            let username: Option<String> = crate::db_guard(&db)
                .query_row("SELECT username FROM users WHERE id = ?1", [user_id], |r| r.get(0))
                .ok();
            if let Some(username) = username {
                crate::channels::deliver_via(&db, &ladder, user_id, &username, &msg);
            }
            stamped(&db, &call_id);
            claim(&in_flight).remove(&call_id);
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
    /// A call id with no row counts as fully applied, so frames for a call
    /// that was deleted are acknowledged and drained on the voice side.
    fn applied(&self, call_id: &str) -> u64 {
        crate::db_guard(&self.db)
            .query_row("SELECT applied_seq FROM voice_calls WHERE id = ?1", [call_id], |r| r.get::<_, i64>(0)).map_or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => u64::MAX,
                _ => 0,
            }, |n| u64::try_from(n).unwrap_or(0))
    }

    fn apply(&self, call_id: &str, seq: u64, body: CallBody) -> Result<(), String> {
        if self.fallback.get().is_none() {
            return Err("the fallback ladder is not set yet".into());
        }
        let now = jiff::Timestamp::now();
        let ended = {
            let conn = crate::db_guard(&self.db);
            let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
            let known = tx
                .execute("UPDATE voice_calls SET applied_seq = ?2 WHERE id = ?1", (call_id, i64::try_from(seq).unwrap_or(i64::MAX)))
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
                        ended = end_call(&tx, call_id, outcome.as_str(), None, now).map_err(|e| e.to_string())?;
                        if let (Some((user_id, _)), Outcome::Failed { reason }) = (&ended, outcome) {
                            let _ = crate::log::record(&tx, Some(*user_id), "voice_call_failed", reason);
                        }
                    }
                    CallBody::Ended => {
                        ended = end_call(&tx, call_id, "failed", None, now).map_err(|e| e.to_string())?;
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
            self.fall_through(call_id, user_id, message);
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
        let stamped: Option<String> = conn
            .query_row("SELECT fell_through_at FROM voice_calls WHERE id = ?1", [&id], |r| r.get(0))
            .unwrap();
        assert!(stamped.is_some(), "the fallthrough is recorded once delivered");
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
    async fn frames_for_an_unknown_call_count_as_applied() {
        let (voice, mock) = rig();
        let h = voice.handler.clone();
        assert_eq!(h.applied("ghost"), u64::MAX);
        h.apply("ghost", 2, CallBody::Ringing).unwrap();
        h.apply("ghost", 3, CallBody::Ended).unwrap();
        assert_eq!(h.applied("ghost"), u64::MAX);
        settle().await;
        assert!(mock.seen().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_call_deleted_mid_ring_drains_the_voice_side() {
        use note_voice_proto::testkit::{eventually, fast, Recording};
        use note_voice_proto::{Dir, MemOutbox, Peer, Role};
        let (voice, mock) = rig();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voice.sock");
        let listener = voice.listen(&path).unwrap();
        let far = Peer::new(fast(Role::Voice), Dir::ToNote, Arc::new(Recording::default()), Box::new(MemOutbox::default()));
        let dialer = tokio::spawn(note_voice_proto::dial_forever(far.clone(), path));
        eventually("the link is up", || voice.is_up()).await;
        let id = voice.start_call(1, &link(), &msg(), jiff::Timestamp::now()).unwrap();
        far.send_call(&id, CallBody::Ringing).unwrap();
        eventually("ringing is applied", || voice.handler.applied(&id) == 1).await;
        crate::db_guard(&voice.db).execute("DELETE FROM users WHERE id = 1", []).unwrap();
        far.send_call(&id, CallBody::Outcome { outcome: Outcome::Missed }).unwrap();
        far.send_call(&id, CallBody::Ended).unwrap();
        eventually("the voice side's outbox drains", || far.pending_calls().is_empty()).await;
        assert!(mock.seen().is_empty());
        dialer.abort();
        listener.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nothing_ends_before_the_fallback_is_set() {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", [])
            .unwrap();
        let voice = Voice::new(Arc::new(Mutex::new(conn)));
        let then = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(60);
        let id = voice.start_call(1, &link(), &msg(), then).unwrap();
        assert!(voice.handler.apply(&id, 1, CallBody::Outcome { outcome: Outcome::Missed }).is_err());
        assert_eq!(voice.handler.applied(&id), 0);
        assert_eq!(voice.sweep(jiff::Timestamp::now()), 0);
        let mock = Arc::new(MockChannel::new("push"));
        voice.set_fallback(vec![mock.clone()]);
        voice.handler.apply(&id, 1, CallBody::Outcome { outcome: Outcome::Missed }).unwrap();
        settle().await;
        assert_eq!(mock.seen().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_start_that_began_ringing_is_left_ringing() {
        let (voice, mock) = rig();
        let then = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(60);
        let id = voice.start_call(1, &link(), &msg(), then).unwrap();
        let now = jiff::Timestamp::now();
        let stale = voice.stale_calls(now);
        assert_eq!(stale, vec![(id.clone(), "starting".to_string())]);
        voice.handler.apply(&id, 1, CallBody::Ringing).unwrap();
        assert_eq!(voice.fail_stale(stale, now), 0);
        let status: String = crate::db_guard(&voice.db)
            .query_row("SELECT state FROM voice_calls WHERE id = ?1", [&id], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "ringing");
        settle().await;
        assert!(mock.seen().is_empty());
    }

    fn frames(voice: &Voice, id: &str) -> Vec<CallBody> {
        let conn = crate::db_guard(&voice.db);
        let mut stmt = conn.prepare("SELECT body FROM voice_frames WHERE call_id = ?1 ORDER BY seq").unwrap();
        stmt.query_map([id], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|b| serde_json::from_str(&b.unwrap()).unwrap())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_ring_nobody_reports_on_is_failed_by_the_sweep_once() {
        let (voice, mock) = rig();
        let then = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(120);
        let id = voice.start_call(1, &link(), &msg(), then).unwrap();
        voice.handler.apply(&id, 1, CallBody::Ringing).unwrap();
        assert_eq!(voice.sweep(jiff::Timestamp::now()), 1);
        assert_eq!(voice.sweep(jiff::Timestamp::now()), 0);
        settle().await;
        assert_eq!(mock.seen().len(), 1);
        assert!(frames(&voice, &id).contains(&CallBody::HangUp), "the voice side is told to stop");
        voice.handler.apply(&id, 2, CallBody::Outcome { outcome: Outcome::Missed }).unwrap();
        voice.handler.apply(&id, 3, CallBody::Ended).unwrap();
        settle().await;
        assert_eq!(mock.seen().len(), 1, "the late outcome does not deliver again");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_fresh_ring_is_left_to_the_voice_side() {
        let (voice, mock) = rig();
        let then = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(60);
        let id = voice.start_call(1, &link(), &msg(), then).unwrap();
        voice.handler.apply(&id, 1, CallBody::Ringing).unwrap();
        assert_eq!(voice.sweep(jiff::Timestamp::now()), 0);
        settle().await;
        assert!(mock.seen().is_empty());
        assert!(!frames(&voice, &id).contains(&CallBody::HangUp));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_ring_waits_for_the_first_to_end() {
        let (voice, _mock) = rig();
        let now = jiff::Timestamp::now();
        let first = voice.start_call(1, &link(), &msg(), now).unwrap();
        assert!(voice.start_call(1, &link(), &msg(), now).is_err(), "starting");
        voice.handler.apply(&first, 1, CallBody::Ringing).unwrap();
        assert!(voice.start_call(1, &link(), &msg(), now).is_err(), "ringing");
        let calls: i64 = crate::db_guard(&voice.db)
            .query_row("SELECT COUNT(*) FROM voice_calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(calls, 1, "a refused ring leaves no row");
        voice.handler.apply(&first, 2, CallBody::Outcome { outcome: Outcome::Missed }).unwrap();
        assert!(voice.start_call(1, &link(), &msg(), now).is_ok());
    }

    struct Unwritable;

    impl note_voice_proto::Outbox for Unwritable {
        fn append(&mut self, _: &str, _: &CallBody) -> std::io::Result<u64> {
            Err(std::io::Error::other("disk full"))
        }
        fn unacked(&self, _: &str, _: u64) -> std::io::Result<Vec<(u64, CallBody)>> {
            Ok(Vec::new())
        }
        fn ack(&mut self, _: &str, _: u64) -> std::io::Result<()> {
            Ok(())
        }
        fn pending_calls(&self) -> std::io::Result<Vec<String>> {
            Ok(Vec::new())
        }
        fn forget(&mut self, _: &str) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_start_that_cannot_be_stored_ends_without_falling_through() {
        let conn = crate::db::open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", [])
            .unwrap();
        let voice =
            Voice::with_outbox(Arc::new(Mutex::new(conn)), PeerConfig::new(Role::Note), Box::new(Unwritable));
        let mock = Arc::new(MockChannel::new("push"));
        voice.set_fallback(vec![mock.clone()]);
        let then = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(60);
        assert!(voice.start_call(1, &link(), &msg(), then).is_err());
        let (state, outcome): (String, String) = crate::db_guard(&voice.db)
            .query_row("SELECT state, outcome FROM voice_calls", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((state.as_str(), outcome.as_str()), ("ended", "failed"));
        assert_eq!(voice.sweep(jiff::Timestamp::now()), 0);
        assert_eq!(voice.redrive(), 0);
        settle().await;
        assert!(mock.seen().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_fallthrough_that_never_landed_is_redriven_once() {
        let (voice, mock) = rig();
        crate::db_guard(&voice.db)
            .execute(
                "INSERT INTO voice_calls (id, user_id, direction, message, state, outcome, ring_by, created_at, ended_at)
                 VALUES ('c1', 1, 'outbound', ?1, 'ended', 'missed', 'x', 'x', 'x')",
                [serde_json::to_string(&msg()).unwrap()],
            )
            .unwrap();
        assert_eq!(voice.redrive(), 1);
        settle().await;
        assert_eq!(mock.seen().len(), 1);
        assert_eq!(voice.redrive(), 0);
        settle().await;
        assert_eq!(mock.seen().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dm_joined_links_the_account() {
        let (voice, _mock) = rig();
        let link_id = {
            let conn = crate::db_guard(&voice.db);
            let id = links::begin(&conn, 1, "@aki:t", jiff::Timestamp::now()).unwrap();
            links::set_room(&conn, id, "!r:t").unwrap();
            id
        };
        let join = |room: &str| {
            voice.handler.request(note_voice_proto::Request::DmJoined { link_id, room_id: room.into() })
        };
        assert_eq!(join("!old:t").await, Ok(note_voice_proto::Reply::Done));
        assert!(links::ringable(&crate::db_guard(&voice.db), 1).unwrap().is_none());
        assert_eq!(join("!r:t").await, Ok(note_voice_proto::Reply::Done));
        assert!(links::ringable(&crate::db_guard(&voice.db), 1).unwrap().is_some());
    }
}
