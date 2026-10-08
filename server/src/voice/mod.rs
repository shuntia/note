pub mod call;
pub mod links;
pub mod outbox;
pub mod web;

use crate::channels::{Channel, OutboundMessage};
use note_voice_proto::{
    BoxFuture, CallBody, Dir, Direction, Handler, Media, Origin, Outcome, Pcm, Peer, PeerConfig, Refusal, RefusalCode,
    Reply, Request, Role, VoiceOption, VoiceProfile,
};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

pub const RING_SECS: u32 = 30;
pub const RING_BY_SECS: i64 = 10;
pub const STALE_START_SECS: i64 = 20;
pub const STALE_RING_SECS: i64 = 90;
pub const STALE_LIVE_SECS: i64 = 40 * 60;

type Ladder = Arc<OnceLock<Vec<Arc<dyn Channel>>>>;

/// What Note says on a call. `on_frame` sees every frame of a live call, and
/// every `Outcome` and `Ended`, after it is stored; it is never called while
/// the DB guard is held.
pub trait Conversation: Send + Sync {
    fn on_frame(&self, call_id: &str, body: &CallBody);
}

/// The user already has a call starting, ringing or live.
#[derive(Debug, thiserror::Error)]
#[error("a ring is already under way")]
pub struct RingBusy;

pub struct Voice {
    db: Arc<Mutex<Connection>>,
    peer: Peer,
    handler: Arc<NoteHandler>,
    fallback: Ladder,
    calls: Arc<call::CallManager>,
    web: Arc<web::Relays>,
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
        let cell: Arc<OnceLock<Peer>> = Arc::default();
        let to_voice = cell.clone();
        let calls = Arc::new(call::CallManager::new(Arc::new(move |call_id: &str, body| {
            let Some(peer) = to_voice.get() else { return };
            if let Err(e) = peer.send_call(call_id, body) {
                eprintln!("voice: journaling a reply for {call_id} failed: {e}");
            }
        })));
        let web = Arc::new(web::Relays::default());
        let handler = Arc::new(NoteHandler {
            db: db.clone(),
            fallback: fallback.clone(),
            in_flight: Arc::default(),
            conversation: calls.clone(),
            to_voice: cell.clone(),
            config_dir: Arc::default(),
            web: web.clone(),
        });
        let peer = Peer::new(cfg, Dir::ToVoice, handler.clone(), outbox);
        let _ = cell.set(peer.clone());
        Arc::new(Voice { db, peer, handler, fallback, calls, web })
    }

    /// What a live call needs to hold a conversation; until set, an answered
    /// call stays silent.
    pub fn set_calls(&self, deps: call::CallDeps) {
        let _ = self.handler.config_dir.set(deps.config_dir.clone());
        self.calls.set_deps(deps);
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
    /// also takes up the calls an earlier run left live and re-delivers what
    /// it ended but never delivered.
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
                        v.calls.resume();
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
            other => Err(Refusal::new(RefusalCode::Failed, format!("unexpected reply {other:?}"))),
        }
    }

    pub async fn voices(&self, language: &str) -> Result<Vec<VoiceOption>, Refusal> {
        match self.peer.request(Request::ListVoices { language: language.to_string() }).await? {
            Reply::Voices { voices } => Ok(voices),
            other => Err(Refusal::new(RefusalCode::Failed, format!("unexpected reply {other:?}"))),
        }
    }

    /// The WAV bytes of a short sample of `voice`.
    pub async fn preview(&self, language: &str, voice: &str) -> Result<Vec<u8>, Refusal> {
        use base64::Engine as _;
        let request = Request::Preview { language: language.to_string(), voice: voice.to_string() };
        match self.peer.request(request).await? {
            Reply::Audio { wav_base64 } => base64::engine::general_purpose::STANDARD
                .decode(wav_base64)
                .map_err(|e| Refusal::new(RefusalCode::Failed, format!("the sample is not base64: {e}"))),
            other => Err(Refusal::new(RefusalCode::Failed, format!("unexpected reply {other:?}"))),
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
                "SELECT EXISTS (SELECT 1 FROM voice_calls WHERE user_id = ?1 AND state != 'ended')",
                [user_id],
                |r| r.get(0),
            )?;
            if busy {
                return Err(RingBusy.into());
            }
            conn.execute(
                "INSERT INTO voice_calls (id, user_id, direction, message, state, ring_by, created_at)
                 VALUES (?1, ?2, 'outbound', ?3, 'starting', ?4, ?5)",
                (&id, user_id, serde_json::to_string(msg)?, ring_by.to_string(), now.to_string()),
            )?;
        }
        let voice = self.handler.profile(user_id);
        let sent = self.peer.send_call(
            &id,
            CallBody::Start {
                user_id,
                room_id,
                mxid: link.mxid.clone(),
                title: msg.title.clone(),
                ring_secs: RING_SECS,
                ring_by_ms: ring_by.as_millisecond(),
                voice,
                direction: Direction::Outbound,
                origin: Origin::Matrix,
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
        self.calls.warm_up(user_id, msg);
        Ok(id)
    }

    /// Opens a web call for `user_id`, replacing any web call they hold; a live phone call refuses it.
    pub fn start_web_call(
        &self,
        user_id: i64,
        web::WebStart { thread, message: outbound }: web::WebStart,
        now: jiff::Timestamp,
    ) -> Result<web::WebCall, web::WebRefusal> {
        use web::WebRefusal::{Busy, Unavailable};
        if !self.is_up() {
            return Err(Unavailable);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let ring_by = now + jiff::SignedDuration::from_secs(RING_BY_SECS);
        let direction = if outbound.is_some() { Direction::Outbound } else { Direction::Inbound };
        let message = outbound.as_ref().map(serde_json::to_string).transpose().map_err(|_| Unavailable)?;
        {
            let conn = crate::db_guard(&self.db);
            let busy: bool = conn
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM voice_calls WHERE user_id = ?1 AND state != 'ended' AND origin != 'web')",
                    [user_id],
                    |r| r.get(0),
                )
                .map_err(|_| Unavailable)?;
            if busy {
                return Err(Busy);
            }
            let thread = thread.filter(|thread| {
                conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM conversations WHERE id = ?1 AND user_id = ?2)",
                    (thread, user_id),
                    |r| r.get::<_, bool>(0),
                )
                .unwrap_or(false)
            });
            let dir = if direction == Direction::Outbound { "outbound" } else { "inbound" };
            conn.execute(
                "INSERT INTO voice_calls (id, user_id, direction, message, state, ring_by, created_at, origin, thread_id)
                 VALUES (?1, ?2, ?3, ?4, 'starting', ?5, ?6, 'web', ?7)",
                (&id, user_id, dir, message, ring_by.to_string(), now.to_string(), thread),
            )
            .map_err(|_| Unavailable)?;
        }
        for old in self.web.replace(user_id) {
            self.hang_up(&old);
        }
        let out = self.web.open(&id, user_id);
        let voice = self.handler.profile(user_id);
        let title = crate::text::call_title(crate::text::Lang::from_setting(&voice.language).unwrap_or_default());
        let sent = self.peer.send_call(
            &id,
            CallBody::Start {
                user_id,
                room_id: String::new(),
                mxid: String::new(),
                title,
                ring_secs: 0,
                ring_by_ms: ring_by.as_millisecond(),
                voice,
                direction,
                origin: Origin::Web,
            },
        );
        if sent.is_err() {
            self.web.forget(&id);
            let ended = end_call(&crate::db_guard(&self.db), &id, "failed", None, now).ok().flatten();
            if let Some((user_id, message)) = ended {
                self.handler.fall_through(&id, user_id, message);
            }
            return Err(Unavailable);
        }
        if let Some(msg) = &outbound {
            self.calls.warm_up(user_id, msg);
        }
        Ok(web::WebCall { call_id: id, out })
    }

    pub fn web_audio(&self, call_id: &str, pcm: Vec<i16>) -> bool {
        self.peer.send_media(call_id, Media::AudioIn { pcm: Pcm(pcm) })
    }

    /// Asks the voice side to end the call; it plays out what Note is saying first.
    pub fn hang_up(&self, call_id: &str) {
        if let Err(e) = self.peer.send_call(call_id, CallBody::HangUp) {
            eprintln!("voice: journaling a hang-up for {call_id} failed: {e}");
        }
    }

    pub fn offer_ring(&self, user_id: i64, conversation_id: Option<i64>) -> String {
        self.web.offer_ring_at(user_id, conversation_id, Instant::now())
    }

    /// The call an answered ring opens: in its thread, opening with Note's newest reply there.
    pub fn answer_ring(&self, token: &str, user_id: i64) -> Option<web::WebStart> {
        let thread = self.web.take_ring_at(token, user_id, Instant::now())?;
        let message = thread.and_then(|conversation_id| self.ring_message(user_id, conversation_id));
        Some(web::WebStart { thread, message })
    }

    fn ring_message(&self, user_id: i64, conversation_id: i64) -> Option<OutboundMessage> {
        let text = {
            let conn = crate::db_guard(&self.db);
            let owned: bool = conn
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM conversations WHERE id = ?1 AND user_id = ?2)",
                    (conversation_id, user_id),
                    |r| r.get(0),
                )
                .ok()?;
            if !owned {
                return None;
            }
            match crate::talk::history(&conn, conversation_id, 1).ok()?.pop()? {
                crate::providers::Message::Assistant { text, .. } if !text.trim().is_empty() => text,
                _ => return None,
            }
        };
        let voice = self.handler.profile(user_id);
        Some(OutboundMessage {
            title: crate::text::call_title(crate::text::Lang::from_setting(&voice.language).unwrap_or_default()),
            body: text,
            urgency: crate::channels::Urgency::Normal,
            checkin: false,
            event_id: None,
            conversation_id: Some(conversation_id),
            actions: Vec::new(),
        })
    }

    /// Fails every call the voice side never took up, never reported the end
    /// of, or that stayed live past `STALE_LIVE_SECS`, and returns how many.
    /// Does nothing until the fallback is set.
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
            let secs = match state {
                "ringing" => STALE_RING_SECS,
                "answered" => STALE_LIVE_SECS,
                _ => STALE_START_SECS,
            };
            now - jiff::SignedDuration::from_secs(secs)
        };
        let conn = crate::db_guard(&self.db);
        let Ok(mut stmt) = conn.prepare("SELECT id, state, ring_by FROM voice_calls WHERE state != 'ended'") else {
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
                self.handler.conversation.on_frame(&id, &CallBody::Ended);
                if self.web.is_web(&id) {
                    self.web.end(&id, "failed", call_conversation(&self.db, &id));
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

/// A call whose message was spoken into a thread does not fall through: it
/// is stamped delivered, unless the call bowed out. True when it was stamped.
fn held_conversation(conn: &Connection, call_id: &str, now: jiff::Timestamp) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "UPDATE voice_calls SET fell_through_at = ?2 WHERE id = ?1 AND conversation_id IS NOT NULL AND bowed_out = 0",
        (call_id, now.to_string()),
    )?;
    Ok(n > 0)
}

fn call_conversation(db: &Mutex<Connection>, call_id: &str) -> Option<i64> {
    crate::db_guard(db)
        .query_row("SELECT conversation_id FROM voice_calls WHERE id = ?1", [call_id], |r| r.get(0))
        .ok()
        .flatten()
}

fn state(conn: &Connection, call_id: &str) -> Result<Option<String>, String> {
    use rusqlite::OptionalExtension;
    conn.query_row("SELECT state FROM voice_calls WHERE id = ?1", [call_id], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())
}

fn claim(set: &Mutex<HashSet<String>>) -> std::sync::MutexGuard<'_, HashSet<String>> {
    set.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) struct NoteHandler {
    db: Arc<Mutex<Connection>>,
    fallback: Ladder,
    in_flight: Arc<Mutex<HashSet<String>>>,
    conversation: Arc<dyn Conversation>,
    to_voice: Arc<OnceLock<Peer>>,
    config_dir: Arc<OnceLock<PathBuf>>,
    web: Arc<web::Relays>,
}

impl NoteHandler {
    /// The user's call voice; the defaults when their config cannot be read.
    fn profile(&self, user_id: i64) -> VoiceProfile {
        let Some(config_dir) = self.config_dir.get() else {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| eprintln!("voice: no config dir yet; calls use the default voice"));
            return VoiceProfile::default();
        };
        let username: Option<String> = crate::db_guard(&self.db)
            .query_row("SELECT username FROM users WHERE id = ?1", [user_id], |r| r.get(0))
            .ok();
        let Some(username) = username else { return VoiceProfile::default() };
        let lang = crate::text::Lang::for_user(config_dir, &username);
        crate::config::UserConfig::load(config_dir, &username).map_or_else(
            |_| VoiceProfile { language: lang.code().into(), ..VoiceProfile::default() },
            |cfg| cfg.voice_profile(lang),
        )
    }

    /// Opens a call for the linked user calling in `room_id`, once per `key`,
    /// then hands the voice side its `Start`.
    fn incoming_call(&self, room_id: &str, mxid: &str, key: &str, now: jiff::Timestamp) -> Result<Reply, Refusal> {
        use rusqlite::OptionalExtension;
        let failed = |e: &dyn std::fmt::Display| Refusal::new(RefusalCode::Failed, e.to_string());
        let ring_by = now + jiff::SignedDuration::from_secs(RING_BY_SECS);
        let (id, user_id) = {
            let conn = crate::db_guard(&self.db);
            let tx = conn.unchecked_transaction().map_err(|e| failed(&e))?;
            let Some(user_id) = links::linked_user(&tx, room_id, mxid).map_err(|e| failed(&e))? else {
                return Err(Refusal::new(RefusalCode::BadRequest, "no linked account calls from there"));
            };
            let seen: Option<(String, String)> = tx
                .query_row("SELECT id, state FROM voice_calls WHERE inbound_key = ?1", [key], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()
                .map_err(|e| failed(&e))?;
            match seen {
                Some((_, state)) if state == "ended" => {
                    return Err(Refusal::new(RefusalCode::Failed, "that call has ended"));
                }
                Some((id, _)) => return Ok(Reply::Call { call_id: id }),
                None => {}
            }
            let busy: bool = tx
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM voice_calls WHERE user_id = ?1 AND state != 'ended')",
                    [user_id],
                    |r| r.get(0),
                )
                .map_err(|e| failed(&e))?;
            if busy {
                return Err(Refusal::new(RefusalCode::Failed, "a call is already up"));
            }
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at, inbound_key)
                 VALUES (?1, ?2, 'inbound', 'starting', ?3, ?4, ?5)",
                (&id, user_id, ring_by.to_string(), now.to_string(), key),
            )
            .map_err(|e| failed(&e))?;
            tx.commit().map_err(|e| failed(&e))?;
            (id, user_id)
        };
        let voice = self.profile(user_id);
        let start = CallBody::Start {
            user_id,
            room_id: room_id.to_string(),
            mxid: mxid.to_string(),
            title: crate::text::call_title(crate::text::Lang::from_setting(&voice.language).unwrap_or_default()),
            ring_secs: 0,
            ring_by_ms: ring_by.as_millisecond(),
            voice,
            direction: Direction::Inbound,
            origin: Origin::Matrix,
        };
        let sent = match self.to_voice.get() {
            Some(peer) => peer.send_call(&id, start).map_err(|e| failed(&e)),
            None => Err(Refusal::new(RefusalCode::Failed, "the voice link is not set up")),
        };
        if let Err(refusal) = sent {
            let conn = crate::db_guard(&self.db);
            let _ = end_call(&conn, &id, "failed", None, now);
            return Err(refusal);
        }
        Ok(Reply::Call { call_id: id })
    }

    /// Sends the message on through the rest of the ladder. Delivery is
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
        call::spawn_blocking(deliver);
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
            }, |n| n as u64)
    }

    fn apply(&self, call_id: &str, seq: u64, body: CallBody) -> Result<(), String> {
        if self.fallback.get().is_none() {
            return Err("the fallback ladder is not set yet".into());
        }
        let now = jiff::Timestamp::now();
        let (ended, live, known) = {
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
                    CallBody::Outcome { outcome: Outcome::Answered } => {
                        tx.execute(
                            "UPDATE voice_calls SET state = 'answered' WHERE id = ?1 AND state IN ('starting', 'ringing')",
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
                        let answered = state(&tx, call_id)?.as_deref() == Some("answered");
                        let outcome = if answered { "answered" } else { "failed" };
                        ended = end_call(&tx, call_id, outcome, None, now).map_err(|e| e.to_string())?;
                        if answered && ended.is_some() && held_conversation(&tx, call_id, now).map_err(|e| e.to_string())? {
                            ended = None;
                        }
                        tx.execute("DELETE FROM voice_frames WHERE call_id = ?1", [call_id])
                            .map_err(|e| e.to_string())?;
                    }
                    CallBody::Start { .. }
                    | CallBody::HangUp
                    | CallBody::Speak { .. }
                    | CallBody::SpeakDone { .. }
                    | CallBody::Play { .. }
                    | CallBody::Drop { .. }
                    | CallBody::Draft { .. }
                    | CallBody::Commit { .. }
                    | CallBody::Retract { .. }
                    | CallBody::Floor { .. }
                    | CallBody::BargeIn { .. }
                    | CallBody::Played { .. } => {}
                }
            }
            let live = known && state(&tx, call_id)?.as_deref() == Some("answered");
            tx.commit().map_err(|e| e.to_string())?;
            (ended, live, known)
        };
        if live || (known && matches!(body, CallBody::Outcome { .. } | CallBody::Ended)) {
            self.conversation.on_frame(call_id, &body);
        }
        if known {
            self.web.on_frame(call_id, &body, || call_conversation(&self.db, call_id));
        }
        if let Some((user_id, message)) = ended {
            self.fall_through(call_id, user_id, message);
        }
        Ok(())
    }

    fn media(&self, call_id: &str, body: Media) {
        self.web.media(call_id, body);
    }

    fn request(&self, body: Request) -> BoxFuture<Result<Reply, Refusal>> {
        let reply = match body {
            Request::DmJoined { link_id, room_id } => {
                let conn = crate::db_guard(&self.db);
                links::mark_joined(&conn, link_id, &room_id, jiff::Timestamp::now())
                    .map(|_| Reply::Done)
                    .map_err(|e| Refusal::new(RefusalCode::Failed, e.to_string()))
            }
            Request::IncomingCall { room_id, mxid, key } => {
                self.incoming_call(&room_id, &mxid, &key, jiff::Timestamp::now())
            }
            Request::OpenDm { .. } => Err(Refusal::new(RefusalCode::BadRequest, "Note does not open rooms")),
            Request::ListVoices { .. } | Request::Preview { .. } => {
                Err(Refusal::new(RefusalCode::BadRequest, "Note holds no voices"))
            }
        };
        Box::pin(async move { reply })
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
            checkin: false,
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
        assert!(voice.start_call(1, &link(), &msg(), now).unwrap_err().is::<RingBusy>(), "starting");
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

    fn answered(voice: &Voice) -> String {
        let id = voice.start_call(1, &link(), &msg(), jiff::Timestamp::now()).unwrap();
        voice.handler.apply(&id, 1, CallBody::Ringing).unwrap();
        voice.handler.apply(&id, 2, CallBody::Outcome { outcome: Outcome::Answered }).unwrap();
        id
    }

    struct Talk {
        voice: Arc<Voice>,
        mock: Arc<MockChannel>,
        llm: Arc<crate::providers::mock::MockLLM>,
        _dir: tempfile::TempDir,
    }

    /// A rig whose calls hold a conversation on `rounds`; completions alone never wake it.
    fn talking(rounds: Vec<Vec<crate::providers::mock::StreamPiece>>) -> Talk {
        talking_on(crate::providers::mock::MockLLM::streamed(rounds))
    }

    fn talking_on(llm: crate::providers::mock::MockLLM) -> Talk {
        let (voice, mock) = rig();
        let dir = tempfile::tempdir().unwrap();
        let prompts = dir.path().join("defaults/prompts");
        std::fs::create_dir_all(&prompts).unwrap();
        std::fs::write(
            dir.path().join("defaults/user.toml"),
            "display_name = \"Aki\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
        )
        .unwrap();
        let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../config/defaults/prompts/voice.md");
        std::fs::copy(shipped, prompts.join("voice.md")).unwrap();
        let llm = Arc::new(llm);
        voice.set_calls(call::CallDeps {
            db: voice.db.clone(),
            config_dir: dir.path().to_path_buf(),
            data_dir: dir.path().to_path_buf(),
            llm: llm.clone(),
            voice_llm: llm.clone(),
            embeddings: None,
            search: None,
            settings: call::CallSettings { wake_settle: std::time::Duration::from_secs(60), ..Default::default() },
        });
        Talk { voice, mock, llm, _dir: dir }
    }

    /// Answers a rung call and waits for its opening to be sent.
    async fn answered_and_spoken(t: &Talk) -> String {
        let id = t.voice.start_call(1, &link(), &msg(), jiff::Timestamp::now()).unwrap();
        let llm = t.llm.clone();
        note_voice_proto::testkit::eventually("the warm-up", || llm.seen().len() == 1).await;
        t.voice.handler.apply(&id, 1, CallBody::Ringing).unwrap();
        t.voice.handler.apply(&id, 2, CallBody::Outcome { outcome: Outcome::Answered }).unwrap();
        note_voice_proto::testkit::eventually("the opening", || frames(&t.voice, &id).contains(&CallBody::Play { reply: 1 }))
            .await;
        id
    }

    fn call_conversation(voice: &Voice, id: &str) -> i64 {
        crate::db_guard(&voice.db)
            .query_row("SELECT conversation_id FROM voice_calls WHERE id = ?1", [id], |r| r.get(0))
            .unwrap()
    }

    fn thread(voice: &Voice, conv: i64) -> Vec<(String, String)> {
        let conn = crate::db_guard(&voice.db);
        let mut stmt = conn.prepare("SELECT role, content FROM talk_messages WHERE conversation_id = ?1 ORDER BY id").unwrap();
        stmt.query_map([conv], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_answered_call_stays_live_and_speaks_the_message() {
        let t = talking(vec![vec![]]);
        let id = answered_and_spoken(&t).await;
        let state: String = crate::db_guard(&t.voice.db)
            .query_row("SELECT state FROM voice_calls WHERE id = ?1", [&id], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "answered");
        let sent = frames(&t.voice, &id);
        assert!(sent.contains(&CallBody::Speak { reply: 1, idx: 0, text: "how is the essay going?".into() }), "{sent:?}");
        assert!(sent.contains(&CallBody::SpeakDone { reply: 1 }));
        settle().await;
        assert!(t.mock.seen().is_empty(), "nothing falls through while the call is live");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_voice_model_that_does_not_stream_is_not_warmed_up() {
        let t = talking_on(crate::providers::mock::MockLLM::scripted(vec![]));
        t.voice.start_call(1, &link(), &msg(), jiff::Timestamp::now()).unwrap();
        settle().await;
        assert!(t.llm.seen().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_answered_call_runs_the_conversation() {
        use crate::providers::mock::StreamPiece::Text;
        let t = talking(vec![vec![], vec![Text("Glad to hear it.")]]);
        let id = answered_and_spoken(&t).await;
        assert!(t.llm.seen()[0].system.contains("You called about: Check-in."), "the warm-up sends the brief");
        t.voice.handler.apply(&id, 3, CallBody::Commit { turn: 1, text: "thanks".into(), language: None }).unwrap();
        let v = t.voice.clone();
        let reply = CallBody::Speak { reply: 2, idx: 0, text: "Glad to hear it.".into() };
        note_voice_proto::testkit::eventually("the reply", || frames(&v, &id).contains(&reply)).await;
        let conv = call_conversation(&t.voice, &id);
        let v = t.voice.clone();
        note_voice_proto::testkit::eventually("the reply is recorded", || thread(&v, conv).len() == 3).await;
        let row = |role: &str, text: &str| (role.to_string(), text.to_string());
        assert_eq!(
            thread(&t.voice, conv),
            vec![row("assistant", "how is the essay going?"), row("user", "thanks"), row("assistant", "Glad to hear it.")]
        );
        let via = crate::talk::via_of(&crate::db_guard(&t.voice.db), conv).unwrap();
        assert_eq!(via, crate::talk::Via::Voice);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_ring_waits_for_an_answered_call_to_end() {
        let (voice, _mock) = rig();
        let id = answered(&voice);
        assert!(voice.start_call(1, &link(), &msg(), jiff::Timestamp::now()).unwrap_err().is::<RingBusy>());
        voice.handler.apply(&id, 3, CallBody::Ended).unwrap();
        assert!(voice.start_call(1, &link(), &msg(), jiff::Timestamp::now()).is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ended_after_answered_records_answered_and_does_not_fall_through() {
        let t = talking(vec![vec![]]);
        let id = answered_and_spoken(&t).await;
        t.voice.handler.apply(&id, 3, CallBody::Ended).unwrap();
        settle().await;
        assert!(t.mock.seen().is_empty(), "the message was spoken and is in the thread");
        let (state, outcome): (String, String) = crate::db_guard(&t.voice.db)
            .query_row("SELECT state, outcome FROM voice_calls WHERE id = ?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((state.as_str(), outcome.as_str()), ("ended", "answered"));
        assert_eq!(t.voice.redrive(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_answered_call_does_not_fall_through() {
        let t = talking(vec![vec![], vec![]]);
        let id = answered_and_spoken(&t).await;
        t.voice.handler.apply(&id, 3, CallBody::Ended).unwrap();
        let reborn = Voice::new(t.voice.db.clone());
        reborn.set_fallback(vec![t.mock.clone()]);
        assert_eq!(reborn.redrive(), 0, "a restart does not deliver it either");

        let missed = t.voice.start_call(1, &link(), &msg(), jiff::Timestamp::now()).unwrap();
        t.voice.handler.apply(&missed, 1, CallBody::Outcome { outcome: Outcome::Missed }).unwrap();
        settle().await;
        assert_eq!(t.mock.seen().len(), 1, "only the missed call falls through");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_answered_call_that_never_held_a_conversation_falls_through_once() {
        let (voice, mock) = rig();
        let id = answered(&voice);
        voice.handler.apply(&id, 3, CallBody::Ended).unwrap();
        settle().await;
        assert_eq!(mock.seen().len(), 1);
        assert_eq!(voice.redrive(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_answered_call_that_bowed_out_falls_through_once() {
        use crate::providers::mock::StreamPiece::Fail;
        let t = talking((0..5).map(|i| if i == 0 { vec![] } else { vec![Fail("status 400")] }).collect());
        let id = answered_and_spoken(&t).await;
        t.voice.handler.apply(&id, 3, CallBody::Commit { turn: 1, text: "hello?".into(), language: None }).unwrap();
        let v = t.voice.clone();
        note_voice_proto::testkit::eventually("the apology", || frames(&v, &id).contains(&CallBody::Play { reply: 3 })).await;
        t.voice.handler.apply(&id, 4, CallBody::Commit { turn: 2, text: "are you there?".into(), language: None }).unwrap();
        note_voice_proto::testkit::eventually("the bow-out", || frames(&v, &id).contains(&CallBody::HangUp)).await;
        let bowed: bool = crate::db_guard(&t.voice.db)
            .query_row("SELECT bowed_out FROM voice_calls WHERE id = ?1", [&id], |r| r.get(0))
            .unwrap();
        assert!(bowed, "recorded before the hang-up goes out");
        t.voice.handler.apply(&id, 5, CallBody::Ended).unwrap();
        settle().await;
        assert_eq!(t.mock.seen().len(), 1, "the promised message is sent");
        assert_eq!(t.voice.redrive(), 0);
        let reborn = Voice::new(t.voice.db.clone());
        reborn.set_fallback(vec![t.mock.clone()]);
        assert_eq!(reborn.redrive(), 0, "a restart does not send it again");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_frame_before_the_first_sweep_resumes_its_call() {
        use crate::providers::mock::StreamPiece::Text;
        let t = talking(vec![vec![Text("Still here.")]]);
        {
            let conn = crate::db_guard(&t.voice.db);
            let now = jiff::Timestamp::now();
            let conv = crate::talk::create(&conn, 1, "Call", now).unwrap();
            conn.execute(
                "INSERT INTO voice_calls (id, user_id, direction, message, state, ring_by, created_at, conversation_id)
                 VALUES ('c1', 1, 'outbound', ?1, 'answered', ?2, ?2, ?3)",
                (serde_json::to_string(&msg()).unwrap(), now.to_string(), conv),
            )
            .unwrap();
        }
        assert!(!t.voice.calls.is_live("c1"));
        t.voice.handler.apply("c1", 5, CallBody::Commit { turn: 2, text: "hello".into(), language: None }).unwrap();
        assert!(t.voice.calls.is_live("c1"));
        let llm = t.llm.clone();
        note_voice_proto::testkit::eventually("the reply model is asked", || !llm.seen().is_empty()).await;
        let Some(crate::providers::Message::User(block)) = t.llm.seen()[0].messages.last().cloned() else {
            panic!("{:?}", t.llm.seen()[0].messages)
        };
        assert!(block.contains("[note] Note restarted; the call is still on"), "{block}");
        assert!(block.ends_with("[you] hello"), "{block}");
        assert_eq!(t.voice.calls.resume(), 0, "the sweeper does not resume it again");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_note_restart_resumes_a_live_call() {
        use crate::providers::mock::StreamPiece::Text;
        let t = talking(vec![vec![Text("Still here.")]]);
        let conv = {
            let conn = crate::db_guard(&t.voice.db);
            let now = jiff::Timestamp::now();
            let conv = crate::talk::create(&conn, 1, "Call", now).unwrap();
            crate::talk::append_text(&conn, conv, "assistant", "how is the essay going?", now).unwrap();
            conn.execute(
                "INSERT INTO voice_calls (id, user_id, direction, message, state, ring_by, created_at, conversation_id, last_reply)
                 VALUES ('c1', 1, 'outbound', ?1, 'answered', ?2, ?2, ?3, 2)",
                (serde_json::to_string(&msg()).unwrap(), now.to_string(), conv),
            )
            .unwrap();
            conn.execute(
                "INSERT INTO voice_jobs (call_id, job, reply, call_index, tool, args, state, started_at)
                 VALUES ('c1', 1, 2, 0, 'task_create', '{}', 'running', 'x')",
                [],
            )
            .unwrap();
            conv
        };
        t.voice.spawn_sweeper();
        let calls = t.voice.calls.clone();
        note_voice_proto::testkit::eventually("the call resumes", || calls.is_live("c1")).await;
        t.voice.handler.apply("c1", 5, CallBody::Commit { turn: 2, text: "hello".into(), language: None }).unwrap();
        let llm = t.llm.clone();
        note_voice_proto::testkit::eventually("the reply model is asked", || !llm.seen().is_empty()).await;
        let messages = &t.llm.seen()[0].messages;
        assert!(
            matches!(&messages[0], crate::providers::Message::Assistant { text, .. } if text == "how is the essay going?"),
            "the thread so far is the history: {messages:?}"
        );
        let Some(crate::providers::Message::User(block)) = messages.last() else { panic!("{messages:?}") };
        assert!(block.contains("[job 1 · task_create · interrupted]"), "{block}");
        assert!(block.contains("[note] Note restarted; the call is still on"), "{block}");
        assert!(block.ends_with("[you] hello"), "{block}");
        let v = t.voice.clone();
        let reply = CallBody::Speak { reply: 3, idx: 0, text: "Still here.".into() };
        note_voice_proto::testkit::eventually("the reply", || frames(&v, "c1").contains(&reply)).await;
        assert!(thread(&t.voice, conv).contains(&("assistant".into(), "Still here.".into())));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_live_call_is_left_alone_until_it_outlives_the_live_limit() {
        let (voice, mock) = rig();
        let id = answered(&voice);
        let now = jiff::Timestamp::now();
        assert_eq!(voice.sweep(now + jiff::SignedDuration::from_secs(STALE_RING_SECS + 60)), 0);
        assert_eq!(voice.sweep(now + jiff::SignedDuration::from_secs(STALE_LIVE_SECS + 60)), 1);
        let (state, outcome): (String, String) = crate::db_guard(&voice.db)
            .query_row("SELECT state, outcome FROM voice_calls WHERE id = ?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((state.as_str(), outcome.as_str()), ("ended", "failed"));
        assert!(frames(&voice, &id).contains(&CallBody::HangUp));
        settle().await;
        assert_eq!(mock.seen().len(), 1);
    }

    fn linked(voice: &Voice) {
        let conn = crate::db_guard(&voice.db);
        let id = links::begin(&conn, 1, "@aki:t", jiff::Timestamp::now()).unwrap();
        links::set_room(&conn, id, "!r:t").unwrap();
        links::mark_joined(&conn, id, "!r:t", jiff::Timestamp::now()).unwrap();
    }

    fn incoming(voice: &Voice, mxid: &str, key: &str) -> BoxFuture<Result<Reply, Refusal>> {
        voice.handler.request(Request::IncomingCall { room_id: "!r:t".into(), mxid: mxid.into(), key: key.into() })
    }

    fn call_count(voice: &Voice) -> i64 {
        crate::db_guard(&voice.db).query_row("SELECT COUNT(*) FROM voice_calls", [], |r| r.get(0)).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_incoming_call_opens_a_call_and_starts_it() {
        let (voice, _mock) = rig();
        linked(&voice);
        let Ok(Reply::Call { call_id: id }) = incoming(&voice, "@aki:t", "$ev").await else { panic!("no call") };
        let (direction, state, message): (String, String, Option<String>) = crate::db_guard(&voice.db)
            .query_row("SELECT direction, state, message FROM voice_calls WHERE id = ?1", [&id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!((direction.as_str(), state.as_str(), message), ("inbound", "starting", None));
        let starts: Vec<CallBody> = frames(&voice, &id).into_iter().filter(|b| matches!(b, CallBody::Start { .. })).collect();
        assert!(
            matches!(
                starts.as_slice(),
                [CallBody::Start { user_id: 1, ring_secs: 0, direction: Direction::Inbound, room_id, mxid, .. }]
                    if room_id == "!r:t" && mxid == "@aki:t"
            ),
            "{starts:?}"
        );
        assert_eq!(incoming(&voice, "@aki:t", "$ev").await, Ok(Reply::Call { call_id: id.clone() }));
        assert_eq!(call_count(&voice), 1);
        assert_eq!(frames(&voice, &id).iter().filter(|b| matches!(b, CallBody::Start { .. })).count(), 1);
        voice.handler.apply(&id, 1, CallBody::Outcome { outcome: Outcome::Failed { reason: "x".into() } }).unwrap();
        voice.handler.apply(&id, 2, CallBody::Ended).unwrap();
        assert_eq!(
            incoming(&voice, "@aki:t", "$ev").await,
            Err(Refusal::new(RefusalCode::Failed, "that call has ended"))
        );
        assert_eq!(call_count(&voice), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unlinked_caller_is_refused() {
        let (voice, _mock) = rig();
        let refused = |r: Result<Reply, Refusal>| matches!(r, Err(Refusal { code: RefusalCode::BadRequest, .. }));
        assert!(refused(incoming(&voice, "@aki:t", "$a").await), "no link");
        {
            let conn = crate::db_guard(&voice.db);
            let id = links::begin(&conn, 1, "@aki:t", jiff::Timestamp::now()).unwrap();
            links::set_room(&conn, id, "!r:t").unwrap();
        }
        assert!(refused(incoming(&voice, "@aki:t", "$b").await), "invited, not joined");
        linked(&voice);
        assert!(refused(incoming(&voice, "@other:t", "$c").await), "another account in the room");
        assert_eq!(call_count(&voice), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_incoming_call_while_a_call_is_up_is_refused() {
        let (voice, _mock) = rig();
        linked(&voice);
        voice.start_call(1, &link(), &msg(), jiff::Timestamp::now()).unwrap();
        assert_eq!(
            incoming(&voice, "@aki:t", "$ev").await,
            Err(Refusal::new(RefusalCode::Failed, "a call is already up"))
        );
        assert_eq!(call_count(&voice), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_inbound_call_never_falls_through() {
        let (voice, mock) = rig();
        linked(&voice);
        let Ok(Reply::Call { call_id: id }) = incoming(&voice, "@aki:t", "$ev").await else { panic!("no call") };
        voice.handler.apply(&id, 1, CallBody::Outcome { outcome: Outcome::Failed { reason: "x".into() } }).unwrap();
        voice.handler.apply(&id, 2, CallBody::Ended).unwrap();
        assert_eq!(voice.redrive(), 0);
        settle().await;
        assert!(mock.seen().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_inbound_call_stuck_starting_is_swept_without_delivering() {
        let (voice, mock) = rig();
        linked(&voice);
        let Ok(Reply::Call { call_id: id }) = incoming(&voice, "@aki:t", "$ev").await else { panic!("no call") };
        let later = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(RING_BY_SECS + STALE_START_SECS + 1);
        assert_eq!(voice.sweep(later), 1);
        let (state, outcome): (String, String) = crate::db_guard(&voice.db)
            .query_row("SELECT state, outcome FROM voice_calls WHERE id = ?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((state.as_str(), outcome.as_str()), ("ended", "failed"));
        assert!(frames(&voice, &id).contains(&CallBody::HangUp));
        assert_eq!(voice.redrive(), 0);
        settle().await;
        assert!(mock.seen().is_empty());
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

    #[tokio::test(flavor = "multi_thread")]
    async fn a_web_call_is_refused_while_the_voice_side_is_down() {
        let (voice, _) = rig();
        let start = web::WebStart { thread: None, message: None };
        assert!(matches!(voice.start_web_call(1, start, jiff::Timestamp::now()), Err(web::WebRefusal::Unavailable)));
        let calls: i64 = crate::db_guard(&voice.db).query_row("SELECT COUNT(*) FROM voice_calls", [], |r| r.get(0)).unwrap();
        assert_eq!(calls, 0);
    }

    struct Up {
        _dir: tempfile::TempDir,
        dialer: tokio::task::JoinHandle<()>,
        listener: tokio::task::JoinHandle<()>,
    }

    impl Drop for Up {
        fn drop(&mut self) {
            self.dialer.abort();
            self.listener.abort();
        }
    }

    async fn up(voice: &Arc<Voice>) -> Up {
        use note_voice_proto::testkit::{eventually, fast, Recording};
        use note_voice_proto::{Dir, MemOutbox, Peer, Role};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voice.sock");
        let listener = voice.listen(&path).unwrap();
        let far = Peer::new(fast(Role::Voice), Dir::ToNote, Arc::new(Recording::default()), Box::new(MemOutbox::default()));
        let dialer = tokio::spawn(note_voice_proto::dial_forever(far, path));
        eventually("the link is up", || voice.is_up()).await;
        Up { _dir: dir, dialer, listener }
    }

    fn web_row(voice: &Voice, id: &str) -> (String, String, Option<String>, Option<i64>) {
        crate::db_guard(&voice.db)
            .query_row("SELECT origin, direction, message, thread_id FROM voice_calls WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_web_call_opens_in_the_callers_own_thread_and_waits_for_them() {
        let (voice, _) = rig();
        let _up = up(&voice).await;
        let (mine, theirs) = {
            let conn = crate::db_guard(&voice.db);
            conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aitest', 'x', 'member')", []).unwrap();
            let now = jiff::Timestamp::now();
            (crate::talk::create(&conn, 1, "a", now).unwrap(), crate::talk::create(&conn, 2, "b", now).unwrap())
        };
        let now = jiff::Timestamp::now();
        let call = voice.start_web_call(1, web::WebStart { thread: Some(mine), message: None }, now).unwrap();
        assert_eq!(web_row(&voice, &call.call_id), ("web".into(), "inbound".into(), None, Some(mine)));
        assert!(matches!(
            frames(&voice, &call.call_id).as_slice(),
            [CallBody::Start { user_id: 1, origin: Origin::Web, direction: Direction::Inbound, ring_secs: 0, .. }]
        ));
        let other = voice.start_web_call(1, web::WebStart { thread: Some(theirs), message: None }, now).unwrap();
        assert_eq!(web_row(&voice, &other.call_id).3, None, "another user's thread is not opened");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_answered_ring_is_notes_call_and_replaces_the_open_one() {
        let (voice, _) = rig();
        let _up = up(&voice).await;
        let now = jiff::Timestamp::now();
        let mut first = voice.start_web_call(1, web::WebStart { thread: None, message: None }, now).unwrap();
        let second = voice.start_web_call(1, web::WebStart { thread: None, message: Some(msg()) }, now).unwrap();
        let (origin, direction, message, _) = web_row(&voice, &second.call_id);
        assert_eq!((origin.as_str(), direction.as_str()), ("web", "outbound"));
        assert_eq!(serde_json::from_str::<OutboundMessage>(&message.unwrap()).unwrap().body, msg().body);
        assert_eq!(first.out.try_recv().unwrap(), web::WebOut::Ended { reason: "replaced", conversation_id: None });
        assert!(frames(&voice, &first.call_id).contains(&CallBody::HangUp));
        assert!(voice.web.is_web(&second.call_id) && !voice.web.is_web(&first.call_id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_web_call_is_busy_while_a_phone_call_is_up_and_keeps_the_open_one() {
        let (voice, _) = rig();
        let _up = up(&voice).await;
        let now = jiff::Timestamp::now();
        let mut open = voice.start_web_call(1, web::WebStart { thread: None, message: None }, now).unwrap();
        voice.start_call(1, &link(), &msg(), now).unwrap_err();
        crate::db_guard(&voice.db).execute("UPDATE voice_calls SET origin = 'matrix' WHERE id = ?1", [&open.call_id]).unwrap();
        let refused = voice.start_web_call(1, web::WebStart { thread: None, message: None }, now);
        assert!(matches!(refused, Err(web::WebRefusal::Busy)));
        assert!(open.out.try_recv().is_err(), "a refused call replaces nothing");
        assert!(voice.web.is_web(&open.call_id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_web_calls_frames_reach_its_socket_and_others_are_ignored() {
        let (voice, _) = rig();
        let _up = up(&voice).await;
        let conv = crate::talk::create(&crate::db_guard(&voice.db), 1, "a", jiff::Timestamp::now()).unwrap();
        let mut call = voice.start_web_call(1, web::WebStart { thread: None, message: None }, jiff::Timestamp::now()).unwrap();
        let mut ghost = voice.web.open("ghost", 1);
        let id = call.call_id.clone();
        crate::db_guard(&voice.db).execute("UPDATE voice_calls SET conversation_id = ?2 WHERE id = ?1", (&id, conv)).unwrap();
        let h = voice.handler.clone();
        h.apply("ghost", 1, CallBody::Draft { turn: 1, text: "hi".into(), language: None }).unwrap();
        h.apply("ghost", 2, CallBody::Ended).unwrap();
        h.apply(&id, 1, CallBody::Draft { turn: 1, text: "hi".into(), language: None }).unwrap();
        h.apply(&id, 2, CallBody::Ended).unwrap();
        assert!(ghost.try_recv().is_err(), "a call id with no row is not routed");
        assert_eq!(call.out.try_recv().unwrap(), web::WebOut::Caption("hi".into()));
        assert_eq!(call.out.try_recv().unwrap(), web::WebOut::Ended { reason: "ended", conversation_id: Some(conv) });
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_web_call_is_failed_on_its_socket() {
        let (voice, _) = rig();
        let then = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(120);
        crate::db_guard(&voice.db)
            .execute(
                "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at, origin)
                 VALUES ('w', 1, 'inbound', 'starting', ?1, ?1, 'web')",
                [then.to_string()],
            )
            .unwrap();
        let mut out = voice.web.open("w", 1);
        assert_eq!(voice.sweep(jiff::Timestamp::now()), 1);
        assert_eq!(out.try_recv().unwrap(), web::WebOut::Ended { reason: "failed", conversation_id: None });
        assert!(!voice.web.is_web("w"));
    }
}
