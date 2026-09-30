# Voice Calls, Phase 1: Link and Ring — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Note rings the user's iPhone through Element X for urgent messages, over a Note ↔ voice link that loses nothing and applies nothing twice. After every ring the message still reaches the user through the rest of the delivery ladder.

**Architecture:**
- A new `note-voice-proto` crate holds the pieces both processes use:
  - the frame types and the length-prefixed JSON codec,
  - the durable outbox contract,
  - the `Peer` runtime (hello, heartbeats, requests, acknowledged and replayed call frames),
  - a fault-injecting proxy for tests.
- `note-server` listens on a Unix socket. It keeps its outbound call frames in SQLite and gains a `voice` channel at the top of the ladder.
- A new `note-voice` binary dials the socket and journals its frames to disk. It speaks Matrix (create the DM, ring, detect answer and decline) against the homeserver.

**Tech Stack:** Rust 2021, tokio (Unix sockets), serde/serde_json, rusqlite, reqwest 0.13 (rustls), axum 0.8 (mock homeserver in tests), React/TypeScript (Settings), Nix (crane, NixOS module).

**Spec:** `docs/superpowers/specs/2026-09-29-voice-calls-design.md`

This is phase 1 of 4. Later plans cover:
- **Phase 2:** the audio pipeline and LiveKit (VAD, STT, Smart Turn, TTS, cues, voice picker).
- **Phase 3:** the conversation (streaming model, `Call` session kind, brief, `respond`, drafts, interleaving).
- **Phase 4:** inbound calls and the remaining settings.

Phase 1 ships on its own: a working ring with fallthrough, which is the "ring to get attention" behaviour. Answering in phase 1 ends the call at once, because the call carries no content yet.

## Global Constraints

- **Socket.** Unix domain socket only, `/run/note/voice.sock`, mode 0660, group `note`. No TCP.
- **Framing.** A u32 big-endian length, then JSON. 1 MiB maximum per frame.
- **Handshake.** `Hello {proto, role, instance}` both ways; `PROTO_VERSION = 1`. A mismatch closes the connection and neither side rings.
- **Heartbeat.** `Ping`/`Pong` every 1 s; three missed pongs mark the link down. The voice side reconnects with jittered backoff capped at 2 s.
- **Durability.** Every call frame is written durably before it is sent. It is resent on every reconnect until acknowledged. The receiver drops any `seq` it has already applied: delivery is at-least-once, the effect exactly-once.
- **Lock order.** `Peer::send_call` is never called while holding the Note DB guard.
- **Ringing.**
  - Outbound ring lasts 30 s.
  - A `Start` carries `ring_by_ms = now + 10 s`. The voice side refuses a `Start` it receives after that (`Outcome::Failed {reason: "late"}`).
  - Note fails a call still `starting` 20 s after `ring_by` and falls through.
- **Falling through.** Every outcome re-delivers the message through the rest of the ladder, exactly once per call. The first transition to `ended` wins.
- **Matrix.**
  - Every HTTP client sends its own `user-agent` (Cloudflare returns 1010 for some defaults).
  - The DM is unencrypted: `trusted_private_chat`, `is_direct`.
  - The ring recipe is exactly the one in the spec's "Verified groundwork".
- **Per-user setting.** `ring_for` is `"urgent"` (default: messages with `Urgency::High`) or `"never"`.
- **Comments.** Follow `CLAUDE.md`: a comment only at a declaration and only where the signature cannot say it. No process history in code or comments.
- **Commits.** End every commit message with `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`.

## Review Focus

1. **The voice service is down, or restarting, when an urgent check-in fires.** The user expects the push or Telegram message right away and no ring minutes later. Pinned in Task 7 (`down_link_falls_through_at_once`) and Task 9 (`late_start_is_refused_without_touching_matrix`).
2. **Note restarts while the phone is ringing.** The outcome must still land once, and the message must fall through once, not twice. Pinned in Task 7 (`note_restart_mid_ring_applies_the_outcome_once`).
3. **A wrong Matrix ID, or an invite never accepted.** Note must not try to ring an unlinked user; delivery skips the voice channel. Pinned in Task 7 (`invited_but_not_joined_is_not_rung`).
4. **The homeserver is unreachable, or the bot token is revoked, mid-ring.** The user expects the message anyway: the outcome is `Failed` and it falls through. Pinned in Task 9 (`homeserver_error_fails_the_call_and_reports_it`).
5. **The user declines on the lock screen.** It must stop at once and fall through, not ring for 30 s. Pinned in Task 9 (`decline_ends_the_ring`).

---

## File Structure

```
Cargo.toml                         workspace: + voice-proto, voice
voice-proto/Cargo.toml
voice-proto/src/lib.rs             re-exports
voice-proto/src/frame.rs           Frame, Request, Reply, Refusal, CallBody, Outcome, Role, Dir
voice-proto/src/codec.rs           read_frame / write_frame
voice-proto/src/stream.rs          Outbox trait, MemOutbox, classify
voice-proto/src/peer.rs            Peer, PeerConfig, Handler, dial_forever, listen_forever
voice-proto/src/journal.rs         FileOutbox, AppliedFile (voice-side durability)
voice-proto/src/testkit.rs         FaultProxy, Recording, eventually, fast()
voice-proto/tests/link.rs          fault-injection suite
server/Cargo.toml                  + note-voice-proto
server/src/db.rs                   v42: voice_links, voice_calls, voice_frames, voice_ops
server/src/voice/mod.rs            Voice (listener, handler, fallthrough, sweep)
server/src/voice/outbox.rs         SqliteOutbox
server/src/voice/links.rs          link rows
server/src/channels/voice.rs       VoiceChannel, rings_for
server/src/channels/mod.rs         OutboundMessage/Urgency/Action gain serde; `pub mod voice`
server/src/config.rs               VoiceConfig, UserConfig.ring_for
server/src/lib.rs                  AppState.voice, with_voice
server/src/main.rs                 listen on the socket
server/src/api.rs                  /api/voice/link, /api/voice/test, settings fields
server/tests/voice_link.rs         Note-side suite with a fake voice peer
web/src/types.ts, api.ts, views/Settings.tsx   Calls row
voice/Cargo.toml
voice/src/lib.rs
voice/src/config.rs                note-voice.toml
voice/src/state.rs                 state.json (links, calls, since)
voice/src/matrix.rs                Matrix client
voice/src/calls.rs                 one outbound ring
voice/src/service.rs               handler, sync loop, recovery, run()
voice/src/main.rs
voice/tests/common/mod.rs          mock homeserver + fake Note
voice/tests/ring.rs
flake.nix, nix/module.nix          packages.note-voice, services.note.voice
```

---

### Task 1: Protocol crate — frames and codec

**Files:**
- Modify: `Cargo.toml`
- Create: `voice-proto/Cargo.toml`, `voice-proto/src/lib.rs`, `voice-proto/src/frame.rs`, `voice-proto/src/codec.rs`
- Modify: `flake.nix` (source fileset)

**Interfaces:**
- Produces:
  - `note_voice_proto::{Frame, Request, Reply, Refusal, RefusalCode, CallBody, Outcome, Role, Dir, PROTO_VERSION, valid_call_id}`
  - `note_voice_proto::codec::{read_frame, write_frame, CodecError, MAX_FRAME}`

- [ ] **Step 1: Workspace and crate manifest**

`Cargo.toml`:

```toml
[workspace]
members = ["server", "voice-proto", "voice"]
resolver = "2"
```

This is the final list. Until Task 9 creates `voice/`, set `members = ["server", "voice-proto"]`. Task 9, Step 1 adds `"voice"`.

`voice-proto/Cargo.toml`:

```toml
[package]
name = "note-voice-proto"
version = "0.1.0"
edition = "2021"
license = "Unlicense"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tokio = { version = "1", features = ["io-util", "net", "sync", "time", "rt", "macros"] }

[dev-dependencies]
tokio = { version = "1", features = ["full"] }
tempfile = "3"
proptest = "1"
```

In `flake.nix`, widen the server's source fileset so the workspace resolves:

```nix
fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./server ./voice-proto ];
```

Apply this in both `commonArgs` and `tests`. Task 11 adds `./voice`.

- [ ] **Step 2: Write the failing codec tests**

`voice-proto/src/codec.rs` (tests at the bottom; implementation in Step 4):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::*;
    use tokio::io::AsyncWriteExt;

    fn every_frame() -> Vec<Frame> {
        vec![
            Frame::Hello { proto: PROTO_VERSION, role: Role::Voice, instance: "i-1".into() },
            Frame::Ping { n: 7 },
            Frame::Pong { n: 7 },
            Frame::Request { id: 1, body: Request::OpenDm { link_id: 3, mxid: "@a:b".into() } },
            Frame::Request { id: 2, body: Request::DmJoined { link_id: 3, room_id: "!r:b".into() } },
            Frame::Response { id: 1, result: Ok(Reply::Dm { room_id: "!r:b".into() }) },
            Frame::Response { id: 2, result: Ok(Reply::Done) },
            Frame::Response {
                id: 3,
                result: Err(Refusal::new(RefusalCode::Timeout, "no answer in time")),
            },
            Frame::Call {
                call_id: "c-1".into(),
                dir: Dir::ToVoice,
                seq: 1,
                body: CallBody::Start {
                    user_id: 1,
                    room_id: "!r:b".into(),
                    mxid: "@a:b".into(),
                    title: "Check-in".into(),
                    ring_secs: 30,
                    ring_by_ms: 1_790_000_000_000,
                },
            },
            Frame::Call { call_id: "c-1".into(), dir: Dir::ToVoice, seq: 2, body: CallBody::HangUp },
            Frame::Call { call_id: "c-1".into(), dir: Dir::ToNote, seq: 1, body: CallBody::Ringing },
            Frame::Call {
                call_id: "c-1".into(),
                dir: Dir::ToNote,
                seq: 2,
                body: CallBody::Outcome { outcome: Outcome::Failed { reason: "late".into() } },
            },
            Frame::Call { call_id: "c-1".into(), dir: Dir::ToNote, seq: 3, body: CallBody::Ended },
            Frame::Ack { call_id: "c-1".into(), dir: Dir::ToNote, seq: 3 },
            Frame::Resume { call_id: "c-1".into(), dir: Dir::ToVoice, after: 1 },
        ]
    }

    #[tokio::test]
    async fn every_frame_round_trips() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        for f in every_frame() {
            write_frame(&mut a, &f).await.unwrap();
            assert_eq!(read_frame(&mut b).await.unwrap(), Some(f));
        }
    }

    #[tokio::test]
    async fn clean_eof_between_frames_is_none() {
        let (a, mut b) = tokio::io::duplex(64);
        drop(a);
        assert!(read_frame(&mut b).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_oversized_length_is_refused_before_reading_the_body() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_u32((MAX_FRAME + 1) as u32).await.unwrap();
        match read_frame(&mut b).await {
            Err(CodecError::TooLarge(n)) => assert_eq!(n, MAX_FRAME + 1),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_oversized_frame_is_not_written() {
        let (mut a, _b) = tokio::io::duplex(64);
        let huge = Frame::Hello { proto: 1, role: Role::Note, instance: "x".repeat(MAX_FRAME) };
        assert!(matches!(write_frame(&mut a, &huge).await, Err(CodecError::TooLarge(_))));
    }

    #[tokio::test]
    async fn malformed_json_is_an_error() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_u32(3).await.unwrap();
        a.write_all(b"{x}").await.unwrap();
        assert!(matches!(read_frame(&mut b).await, Err(CodecError::Json(_))));
    }

    #[test]
    fn call_ids_are_restricted_to_safe_file_names() {
        assert!(valid_call_id("0b9c7a4e-1f1e-4c1b-9a8e-3d2f1c0b9a8e"));
        assert!(!valid_call_id(""));
        assert!(!valid_call_id("../etc"));
        assert!(!valid_call_id(&"a".repeat(65)));
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p note-voice-proto`
Expected: compile errors (`Frame`, `write_frame` not found).

- [ ] **Step 4: Implement `frame.rs`, `codec.rs`, `lib.rs`**

`voice-proto/src/frame.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Bumped on any change to a frame's shape; both sides must agree exactly.
pub const PROTO_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Note,
    Voice,
}

/// The way a call frame travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    ToVoice,
    ToNote,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Frame {
    Hello { proto: u32, role: Role, instance: String },
    Ping { n: u64 },
    Pong { n: u64 },
    Request { id: u64, body: Request },
    Response { id: u64, result: Result<Reply, Refusal> },
    Call { call_id: String, dir: Dir, seq: u64, body: CallBody },
    /// Cumulative: every frame of the call up to `seq` is applied.
    Ack { call_id: String, dir: Dir, seq: u64 },
    /// Asks the sender to resend every frame of the call after `after`.
    Resume { call_id: String, dir: Dir, after: u64 },
}

/// Requests are idempotent: each carries the key of what it creates or
/// changes, and repeating one returns the first answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Request {
    /// Note → voice: open, or reopen, the DM a link lives in.
    OpenDm { link_id: i64, mxid: String },
    /// Voice → Note: the invited account joined the link's DM.
    DmJoined { link_id: i64, room_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Reply {
    Dm { room_id: String },
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    LinkDown,
    Timeout,
    BadRequest,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub code: RefusalCode,
    pub message: String,
}

impl Refusal {
    pub fn new(code: RefusalCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum CallBody {
    /// Note → voice: ring the linked account, unless `ring_by_ms` has passed.
    Start {
        user_id: i64,
        room_id: String,
        mxid: String,
        title: String,
        ring_secs: u32,
        ring_by_ms: i64,
    },
    /// Note → voice: end the call now, ringing or not.
    HangUp,
    /// Voice → Note: the phone is ringing.
    Ringing,
    /// Voice → Note: how the ring ended.
    Outcome { outcome: Outcome },
    /// Voice → Note: the voice side holds nothing more for this call.
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "o", rename_all = "snake_case")]
pub enum Outcome {
    Answered,
    Declined,
    Missed,
    Failed { reason: String },
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Answered => "answered",
            Outcome::Declined => "declined",
            Outcome::Missed => "missed",
            Outcome::Failed { .. } => "failed",
        }
    }
}

/// Call ids name journal files on the voice side.
pub fn valid_call_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
```

`voice-proto/src/codec.rs` (above the tests):

```rust
use crate::frame::Frame;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME: usize = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame of {0} bytes exceeds the {MAX_FRAME}-byte limit")]
    TooLarge(usize),
    #[error("malformed frame: {0}")]
    Json(#[from] serde_json::Error),
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &Frame) -> Result<(), CodecError> {
    let bytes = serde_json::to_vec(frame)?;
    if bytes.len() > MAX_FRAME {
        return Err(CodecError::TooLarge(bytes.len()));
    }
    w.write_u32(bytes.len() as u32).await?;
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}

/// `None` is a clean end of stream between frames.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Frame>, CodecError> {
    let len = match r.read_u32().await {
        Ok(n) => n as usize,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if len > MAX_FRAME {
        return Err(CodecError::TooLarge(len));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(Some(serde_json::from_slice(&buf)?))
}
```

`voice-proto/src/lib.rs`:

```rust
pub mod codec;
pub mod frame;

pub use frame::*;
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p note-voice-proto`
Expected: 6 passed.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock flake.nix voice-proto
git commit -m "feat(voice-proto): frames and a length-prefixed JSON codec shared by Note and the voice service"
```

---

### Task 2: Outbox contract and sequencing

**Files:**
- Create: `voice-proto/src/stream.rs`
- Modify: `voice-proto/src/lib.rs`

**Interfaces:**
- Consumes: `CallBody` (Task 1).
- Produces:
  - `trait Outbox: Send`, with:
    - `append(&mut self, call_id: &str, body: &CallBody) -> io::Result<u64>`
    - `unacked(&self, call_id: &str, after: u64) -> io::Result<Vec<(u64, CallBody)>>`
    - `ack(&mut self, call_id: &str, upto: u64) -> io::Result<()>`
    - `pending_calls(&self) -> io::Result<Vec<String>>`
    - `forget(&mut self, call_id: &str) -> io::Result<()>`
  - `MemOutbox` (with `impl Outbox for Arc<Mutex<MemOutbox>>`).
  - `enum Arrival { Apply, Duplicate, Gap }` and `fn classify(applied: u64, seq: u64) -> Arrival`.

- [ ] **Step 1: Write the failing tests**

In `voice-proto/src/stream.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn seqs_start_at_one_and_survive_acks() {
        let mut o = MemOutbox::default();
        assert_eq!(o.append("c", &CallBody::Ringing).unwrap(), 1);
        assert_eq!(o.append("c", &CallBody::Ended).unwrap(), 2);
        o.ack("c", 2).unwrap();
        assert!(o.unacked("c", 0).unwrap().is_empty());
        assert!(o.pending_calls().unwrap().is_empty());
        assert_eq!(o.append("c", &CallBody::HangUp).unwrap(), 3, "numbering never restarts");
    }

    #[test]
    fn unacked_is_ordered_and_respects_after() {
        let mut o = MemOutbox::default();
        for _ in 0..5 {
            o.append("c", &CallBody::Ringing).unwrap();
        }
        o.ack("c", 2).unwrap();
        let seqs: Vec<u64> = o.unacked("c", 0).unwrap().into_iter().map(|(s, _)| s).collect();
        assert_eq!(seqs, vec![3, 4, 5]);
        let seqs: Vec<u64> = o.unacked("c", 4).unwrap().into_iter().map(|(s, _)| s).collect();
        assert_eq!(seqs, vec![5]);
    }

    #[test]
    fn forget_drops_the_call_entirely() {
        let mut o = MemOutbox::default();
        o.append("c", &CallBody::Ringing).unwrap();
        o.forget("c").unwrap();
        assert!(o.pending_calls().unwrap().is_empty());
        assert_eq!(o.append("c", &CallBody::Ringing).unwrap(), 1);
    }

    #[test]
    fn classify_names_every_arrival() {
        assert_eq!(classify(0, 1), Arrival::Apply);
        assert_eq!(classify(3, 4), Arrival::Apply);
        assert_eq!(classify(3, 3), Arrival::Duplicate);
        assert_eq!(classify(3, 1), Arrival::Duplicate);
        assert_eq!(classify(3, 5), Arrival::Gap);
    }

    proptest! {
        /// Any mix of duplicates and replays, fed through `classify`, applies
        /// each seq exactly once and in order.
        #[test]
        fn classify_applies_each_seq_once(order in prop::collection::vec(1u64..20, 1..200)) {
            let mut applied = 0u64;
            let mut seen = Vec::new();
            for seq in order {
                if classify(applied, seq) == Arrival::Apply {
                    applied = seq;
                    seen.push(seq);
                }
            }
            let expected: Vec<u64> = (1..=applied).collect();
            prop_assert_eq!(seen, expected);
        }
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p note-voice-proto stream`
Expected: compile errors (`MemOutbox`, `classify` not found).

- [ ] **Step 3: Implement**

`voice-proto/src/stream.rs` (above the tests):

```rust
use crate::frame::CallBody;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::{Arc, Mutex};

/// Durable store of the call frames this side sent and the peer has not yet
/// acknowledged. `append` must be durable before it returns.
pub trait Outbox: Send {
    /// Stores `body` as the next frame of `call_id` and returns its seq; the
    /// first is 1 and numbering never restarts while the call is known.
    fn append(&mut self, call_id: &str, body: &CallBody) -> io::Result<u64>;
    fn unacked(&self, call_id: &str, after: u64) -> io::Result<Vec<(u64, CallBody)>>;
    /// Drops every frame of `call_id` up to and including `upto`.
    fn ack(&mut self, call_id: &str, upto: u64) -> io::Result<()>;
    fn pending_calls(&self) -> io::Result<Vec<String>>;
    fn forget(&mut self, call_id: &str) -> io::Result<()>;
}

#[derive(Default)]
struct MemCall {
    last: u64,
    frames: BTreeMap<u64, CallBody>,
}

#[derive(Default)]
pub struct MemOutbox {
    calls: HashMap<String, MemCall>,
}

impl Outbox for MemOutbox {
    fn append(&mut self, call_id: &str, body: &CallBody) -> io::Result<u64> {
        let call = self.calls.entry(call_id.to_string()).or_default();
        call.last += 1;
        call.frames.insert(call.last, body.clone());
        Ok(call.last)
    }

    fn unacked(&self, call_id: &str, after: u64) -> io::Result<Vec<(u64, CallBody)>> {
        Ok(self
            .calls
            .get(call_id)
            .map(|c| c.frames.range(after + 1..).map(|(s, b)| (*s, b.clone())).collect())
            .unwrap_or_default())
    }

    fn ack(&mut self, call_id: &str, upto: u64) -> io::Result<()> {
        if let Some(c) = self.calls.get_mut(call_id) {
            c.frames.retain(|s, _| *s > upto);
        }
        Ok(())
    }

    fn pending_calls(&self) -> io::Result<Vec<String>> {
        let mut ids: Vec<String> =
            self.calls.iter().filter(|(_, c)| !c.frames.is_empty()).map(|(id, _)| id.clone()).collect();
        ids.sort();
        Ok(ids)
    }

    fn forget(&mut self, call_id: &str) -> io::Result<()> {
        self.calls.remove(call_id);
        Ok(())
    }
}

/// Lets tests keep a handle on an outbox a `Peer` owns, the way a restarted
/// process reopens the same journal.
impl Outbox for Arc<Mutex<MemOutbox>> {
    fn append(&mut self, call_id: &str, body: &CallBody) -> io::Result<u64> {
        crate::lock(self).append(call_id, body)
    }
    fn unacked(&self, call_id: &str, after: u64) -> io::Result<Vec<(u64, CallBody)>> {
        crate::lock(self).unacked(call_id, after)
    }
    fn ack(&mut self, call_id: &str, upto: u64) -> io::Result<()> {
        crate::lock(self).ack(call_id, upto)
    }
    fn pending_calls(&self) -> io::Result<Vec<String>> {
        crate::lock(self).pending_calls()
    }
    fn forget(&mut self, call_id: &str) -> io::Result<()> {
        crate::lock(self).forget(call_id)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Arrival {
    Apply,
    Duplicate,
    Gap,
}

pub fn classify(applied: u64, seq: u64) -> Arrival {
    if seq <= applied {
        Arrival::Duplicate
    } else if seq == applied + 1 {
        Arrival::Apply
    } else {
        Arrival::Gap
    }
}
```

In `voice-proto/src/lib.rs`, add:

```rust
pub mod stream;

pub use stream::{classify, Arrival, MemOutbox, Outbox};

/// A lock poisoned by a panic elsewhere still guards intact data.
pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p note-voice-proto`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add voice-proto
git commit -m "feat(voice-proto): the outbox contract and how an arriving seq is classified"
```

---

### Task 3: The `Peer` runtime and the fault-injection suite

**Files:**
- Create: `voice-proto/src/peer.rs`, `voice-proto/src/testkit.rs`, `voice-proto/tests/link.rs`
- Modify: `voice-proto/src/lib.rs`

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces:
  - `type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>`.
  - `trait Handler: Send + Sync + 'static`, with:
    - `applied(&self, call_id: &str) -> u64`
    - `apply(&self, call_id: &str, seq: u64, body: CallBody) -> Result<(), String>`
    - `request(&self, body: Request) -> BoxFuture<Result<Reply, Refusal>>`
    - `acked(&self, call_id: &str, upto: u64)` (default no-op)
    - `link_changed(&self, up: bool)` (default no-op)
  - `struct PeerConfig { role, heartbeat, missed_pongs, hello_timeout, request_timeout }` and `PeerConfig::new(role)`.
  - `Peer::new(cfg: PeerConfig, out_dir: Dir, handler: Arc<dyn Handler>, outbox: Box<dyn Outbox>) -> Peer`, with:
    - `serve(&self, UnixStream) -> Disconnect`
    - `is_up(&self) -> bool`
    - `up_watch(&self) -> watch::Receiver<bool>`
    - `request(&self, Request) -> Result<Reply, Refusal>`
    - `send_call(&self, call_id: &str, body: CallBody) -> io::Result<u64>`
    - `pending_calls(&self) -> Vec<String>`
    - `forget(&self, call_id: &str) -> io::Result<()>`
  - `async fn dial_forever(peer: Peer, path: PathBuf)` and `async fn listen_forever(peer: Peer, listener: UnixListener)`.
  - Testkit:
    - `FaultProxy::start(listen: PathBuf, target: PathBuf) -> io::Result<FaultProxy>`, with `faults: Arc<Faults>`.
    - `Faults`: `duplicate_calls`, `drop_acks` and `hold` (`AtomicBool` each), `cut_after_calls(n)`, `cut()`.
    - `Recording`, a `Handler` that records applied frames; its `request` answers with a settable closure.
    - `eventually(what: &str, f: impl FnMut() -> bool)`.
    - `fast(role) -> PeerConfig`.

- [ ] **Step 1: Write the testkit (test support, compiled into the crate)**

`voice-proto/src/testkit.rs`:

```rust
use crate::codec::{read_frame, write_frame};
use crate::frame::*;
use crate::peer::{BoxFuture, Handler, PeerConfig};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

/// Short timings so the suites run in well under a second per case.
pub fn fast(role: Role) -> PeerConfig {
    PeerConfig {
        role,
        heartbeat: Duration::from_millis(50),
        missed_pongs: 3,
        hello_timeout: Duration::from_millis(500),
        request_timeout: Duration::from_millis(400),
    }
}

pub async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {what}");
}

type Answer = Box<dyn Fn(Request) -> Result<Reply, Refusal> + Send + Sync>;

/// A handler that keeps every applied frame, standing in for either side.
pub struct Recording {
    applied: Mutex<HashMap<String, u64>>,
    pub seen: Mutex<Vec<(String, u64, CallBody)>>,
    pub requests: Mutex<Vec<Request>>,
    pub acks: Mutex<HashMap<String, u64>>,
    answer: Mutex<Answer>,
}

impl Default for Recording {
    fn default() -> Self {
        Self {
            applied: Mutex::default(),
            seen: Mutex::default(),
            requests: Mutex::default(),
            acks: Mutex::default(),
            answer: Mutex::new(Box::new(|_| Ok(Reply::Done))),
        }
    }
}

impl Recording {
    pub fn answer_with(&self, f: impl Fn(Request) -> Result<Reply, Refusal> + Send + Sync + 'static) {
        *crate::lock(&self.answer) = Box::new(f);
    }

    pub fn seqs(&self, call_id: &str) -> Vec<u64> {
        crate::lock(&self.seen).iter().filter(|(c, _, _)| c == call_id).map(|(_, s, _)| *s).collect()
    }

    pub fn bodies(&self, call_id: &str) -> Vec<CallBody> {
        crate::lock(&self.seen).iter().filter(|(c, _, _)| c == call_id).map(|(_, _, b)| b.clone()).collect()
    }
}

impl Handler for Recording {
    fn applied(&self, call_id: &str) -> u64 {
        crate::lock(&self.applied).get(call_id).copied().unwrap_or(0)
    }

    fn apply(&self, call_id: &str, seq: u64, body: CallBody) -> Result<(), String> {
        crate::lock(&self.seen).push((call_id.to_string(), seq, body));
        crate::lock(&self.applied).insert(call_id.to_string(), seq);
        Ok(())
    }

    fn request(&self, body: Request) -> BoxFuture<Result<Reply, Refusal>> {
        crate::lock(&self.requests).push(body.clone());
        let result = (crate::lock(&self.answer))(body);
        Box::pin(async move { result })
    }

    fn acked(&self, call_id: &str, upto: u64) {
        crate::lock(&self.acks).insert(call_id.to_string(), upto);
    }
}

pub struct Faults {
    pub duplicate_calls: AtomicBool,
    pub drop_acks: AtomicBool,
    /// Swallows every frame in both directions without closing anything.
    pub hold: AtomicBool,
    cut_after: AtomicU64,
    kick: watch::Sender<u64>,
}

impl Faults {
    /// Closes the live connection once `n` more call frames have passed.
    pub fn cut_after_calls(&self, n: u64) {
        self.cut_after.store(n, SeqCst);
    }

    pub fn cut(&self) {
        self.kick.send_modify(|g| *g += 1);
    }
}

/// Sits between the two sides, forwarding decoded frames and breaking the
/// link on demand.
pub struct FaultProxy {
    pub faults: Arc<Faults>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FaultProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FaultProxy {
    pub async fn start(listen: PathBuf, target: PathBuf) -> std::io::Result<FaultProxy> {
        let listener = UnixListener::bind(&listen)?;
        let faults = Arc::new(Faults {
            duplicate_calls: AtomicBool::new(false),
            drop_acks: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            cut_after: AtomicU64::new(0),
            kick: watch::channel(0).0,
        });
        let f = faults.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else { continue };
                let Ok(server) = UnixStream::connect(&target).await else { continue };
                tokio::spawn(pump_pair(client, server, f.clone()));
            }
        });
        Ok(FaultProxy { faults, task })
    }
}

async fn pump_pair(a: UnixStream, b: UnixStream, f: Arc<Faults>) {
    let mut kick = f.kick.subscribe();
    kick.borrow_and_update();
    let (ar, aw) = a.into_split();
    let (br, bw) = b.into_split();
    let calls = Arc::new(AtomicU64::new(0));
    tokio::select! {
        _ = pump(ar, bw, f.clone(), calls.clone()) => {}
        _ = pump(br, aw, f.clone(), calls) => {}
        _ = kick.changed() => {}
    }
}

async fn pump(mut r: OwnedReadHalf, mut w: OwnedWriteHalf, f: Arc<Faults>, calls: Arc<AtomicU64>) {
    while let Ok(Some(frame)) = read_frame(&mut r).await {
        if f.hold.load(SeqCst) {
            continue;
        }
        match &frame {
            Frame::Ack { .. } if f.drop_acks.load(SeqCst) => continue,
            Frame::Call { .. } => {
                let n = calls.fetch_add(1, SeqCst) + 1;
                let limit = f.cut_after.load(SeqCst);
                if limit != 0 && n > limit {
                    f.cut_after.store(0, SeqCst);
                    return;
                }
                if f.duplicate_calls.load(SeqCst) && write_frame(&mut w, &frame).await.is_err() {
                    return;
                }
            }
            _ => {}
        }
        if write_frame(&mut w, &frame).await.is_err() {
            return;
        }
    }
}
```

- [ ] **Step 2: Write the failing suite**

`voice-proto/tests/link.rs`:

```rust
use note_voice_proto::peer::{dial_forever, listen_forever, Peer};
use note_voice_proto::testkit::{eventually, fast, FaultProxy, Recording};
use note_voice_proto::*;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Mutex};
use tokio::net::UnixListener;

struct Side {
    peer: Peer,
    rec: Arc<Recording>,
    outbox: Arc<Mutex<MemOutbox>>,
}

fn side(role: Role, out: Dir, outbox: Arc<Mutex<MemOutbox>>, rec: Arc<Recording>) -> Side {
    let peer = Peer::new(fast(role), out, rec.clone(), Box::new(outbox.clone()));
    Side { peer, rec, outbox }
}

struct Rig {
    _dir: tempfile::TempDir,
    proxy: FaultProxy,
    proxy_path: std::path::PathBuf,
    note: Side,
    voice: Side,
    voice_task: tokio::task::JoinHandle<()>,
    _note_task: tokio::task::JoinHandle<()>,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let note_path = dir.path().join("note.sock");
    let proxy_path = dir.path().join("proxy.sock");
    let note = side(Role::Note, Dir::ToVoice, Arc::default(), Arc::default());
    let voice = side(Role::Voice, Dir::ToNote, Arc::default(), Arc::default());
    let listener = UnixListener::bind(&note_path).unwrap();
    let note_task = tokio::spawn(listen_forever(note.peer.clone(), listener));
    let proxy = FaultProxy::start(proxy_path.clone(), note_path).await.unwrap();
    let voice_task = tokio::spawn(dial_forever(voice.peer.clone(), proxy_path.clone()));
    let (n, v) = (note.peer.clone(), voice.peer.clone());
    eventually("both sides up", || n.is_up() && v.is_up()).await;
    Rig { _dir: dir, proxy, proxy_path, note, voice, voice_task, _note_task: note_task }
}

fn one_to(n: u64) -> Vec<u64> {
    (1..=n).collect()
}

#[tokio::test]
async fn frames_arrive_once_and_in_order() {
    let r = rig().await;
    for _ in 0..200 {
        r.voice.peer.send_call("c1", CallBody::Ringing).unwrap();
    }
    let rec = r.note.rec.clone();
    eventually("200 applied", || rec.seqs("c1").len() == 200).await;
    assert_eq!(r.note.rec.seqs("c1"), one_to(200));
    let v = r.voice.peer.clone();
    eventually("all acknowledged", || v.pending_calls().is_empty()).await;
}

#[tokio::test]
async fn a_cut_mid_stream_resumes_where_it_stopped() {
    let r = rig().await;
    r.proxy.faults.cut_after_calls(50);
    for _ in 0..200 {
        r.voice.peer.send_call("c1", CallBody::Ringing).unwrap();
    }
    let rec = r.note.rec.clone();
    eventually("200 applied after the cut", || rec.seqs("c1").len() >= 200).await;
    assert_eq!(r.note.rec.seqs("c1"), one_to(200));
}

#[tokio::test]
async fn duplicated_frames_apply_once() {
    let r = rig().await;
    r.proxy.faults.duplicate_calls.store(true, SeqCst);
    for _ in 0..100 {
        r.note.peer.send_call("c2", CallBody::HangUp).unwrap();
    }
    let rec = r.voice.rec.clone();
    eventually("100 applied", || rec.seqs("c2").len() >= 100).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(r.voice.rec.seqs("c2"), one_to(100));
}

#[tokio::test]
async fn lost_acks_are_recovered_by_replay_without_reapplying() {
    let r = rig().await;
    r.proxy.faults.drop_acks.store(true, SeqCst);
    for _ in 0..50 {
        r.voice.peer.send_call("c3", CallBody::Ringing).unwrap();
    }
    let rec = r.note.rec.clone();
    eventually("50 applied", || rec.seqs("c3").len() == 50).await;
    assert_eq!(r.voice.peer.pending_calls(), vec!["c3".to_string()]);
    r.proxy.faults.drop_acks.store(false, SeqCst);
    r.proxy.faults.cut();
    let v = r.voice.peer.clone();
    eventually("acks after the reconnect", || v.pending_calls().is_empty()).await;
    assert_eq!(r.note.rec.seqs("c3"), one_to(50));
}

#[tokio::test]
async fn a_restarted_sender_replays_its_durable_outbox() {
    let r = rig().await;
    r.proxy.faults.hold.store(true, SeqCst);
    for _ in 0..30 {
        r.voice.peer.send_call("c4", CallBody::Ringing).unwrap();
    }
    r.voice_task.abort();
    r.proxy.faults.hold.store(false, SeqCst);
    r.proxy.faults.cut();
    let reborn = side(Role::Voice, Dir::ToNote, r.voice.outbox.clone(), r.voice.rec.clone());
    tokio::spawn(dial_forever(reborn.peer.clone(), r.proxy_path.clone()));
    let rec = r.note.rec.clone();
    eventually("30 applied after the restart", || rec.seqs("c4").len() >= 30).await;
    assert_eq!(r.note.rec.seqs("c4"), one_to(30));
}

#[tokio::test]
async fn a_silent_link_is_declared_down_and_comes_back() {
    let r = rig().await;
    r.proxy.faults.hold.store(true, SeqCst);
    let v = r.voice.peer.clone();
    eventually("voice notices the dead link", || !v.is_up()).await;
    r.proxy.faults.hold.store(false, SeqCst);
    let (n, v) = (r.note.peer.clone(), r.voice.peer.clone());
    eventually("both back up", || n.is_up() && v.is_up()).await;
}

#[tokio::test]
async fn requests_round_trip_and_fail_cleanly() {
    let r = rig().await;
    r.voice.rec.answer_with(|req| match req {
        Request::OpenDm { link_id, .. } => Ok(Reply::Dm { room_id: format!("!room{link_id}:t") }),
        _ => Err(Refusal::new(RefusalCode::BadRequest, "wrong way")),
    });
    let got = r.note.peer.request(Request::OpenDm { link_id: 4, mxid: "@a:t".into() }).await;
    assert_eq!(got, Ok(Reply::Dm { room_id: "!room4:t".into() }));

    r.proxy.faults.hold.store(true, SeqCst);
    let got = r.note.peer.request(Request::OpenDm { link_id: 5, mxid: "@a:t".into() }).await;
    assert!(
        matches!(got, Err(Refusal { code: RefusalCode::Timeout | RefusalCode::LinkDown, .. })),
        "{got:?}"
    );
    let n = r.note.peer.clone();
    eventually("note sees the link down", || !n.is_up()).await;
    let got = r.note.peer.request(Request::OpenDm { link_id: 6, mxid: "@a:t".into() }).await;
    assert_eq!(got.unwrap_err().code, RefusalCode::LinkDown);
}

#[tokio::test]
async fn a_version_mismatch_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.sock");
    let note = side(Role::Note, Dir::ToVoice, Arc::default(), Arc::default());
    tokio::spawn(listen_forever(note.peer.clone(), UnixListener::bind(&path).unwrap()));
    let mut s = tokio::net::UnixStream::connect(&path).await.unwrap();
    codec::write_frame(&mut s, &Frame::Hello { proto: 99, role: Role::Voice, instance: "x".into() })
        .await
        .unwrap();
    let first = codec::read_frame(&mut s).await.unwrap();
    assert!(matches!(first, Some(Frame::Hello { .. })));
    let next = codec::read_frame(&mut s).await;
    assert!(matches!(next, Ok(None) | Err(_)), "the connection closes: {next:?}");
    assert!(!note.peer.is_up());
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p note-voice-proto --test link`
Expected: compile errors (`peer` module missing).

- [ ] **Step 4: Implement `peer.rs`**

`voice-proto/src/peer.rs`:

```rust
use crate::codec::{read_frame, write_frame, CodecError};
use crate::frame::*;
use crate::stream::{classify, Arrival, Outbox};
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, watch};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What each side does with what arrives. `apply` must record `seq` as
/// applied in the same durable step as the frame's effect. An `Err` from it
/// drops the connection so the frame is redelivered, so it is for storage
/// failures only: a frame whose content makes no sense is recorded as applied
/// and ignored.
pub trait Handler: Send + Sync + 'static {
    fn applied(&self, call_id: &str) -> u64;
    fn apply(&self, call_id: &str, seq: u64, body: CallBody) -> Result<(), String>;
    fn request(&self, body: Request) -> BoxFuture<Result<Reply, Refusal>>;
    fn acked(&self, _call_id: &str, _upto: u64) {}
    fn link_changed(&self, _up: bool) {}
}

#[derive(Debug, Clone)]
pub struct PeerConfig {
    pub role: Role,
    pub heartbeat: Duration,
    pub missed_pongs: u32,
    pub hello_timeout: Duration,
    pub request_timeout: Duration,
}

impl PeerConfig {
    pub fn new(role: Role) -> Self {
        Self {
            role,
            heartbeat: Duration::from_secs(1),
            missed_pongs: 3,
            hello_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug)]
pub enum Disconnect {
    Eof,
    Codec(String),
    HelloRefused(String),
    HeartbeatLost,
    Protocol(String),
}

struct Shared {
    outbox: Box<dyn Outbox>,
    current: Option<mpsc::UnboundedSender<Frame>>,
}

struct Inner {
    cfg: PeerConfig,
    instance: String,
    out_dir: Dir,
    handler: Arc<dyn Handler>,
    shared: Mutex<Shared>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Reply, Refusal>>>>,
    next_request: AtomicU64,
    generation: AtomicU64,
    up: watch::Sender<bool>,
}

#[derive(Clone)]
pub struct Peer {
    inner: Arc<Inner>,
}

fn instance_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

impl Peer {
    pub fn new(cfg: PeerConfig, out_dir: Dir, handler: Arc<dyn Handler>, outbox: Box<dyn Outbox>) -> Peer {
        Peer {
            inner: Arc::new(Inner {
                cfg,
                instance: instance_id(),
                out_dir,
                handler,
                shared: Mutex::new(Shared { outbox, current: None }),
                pending: Mutex::new(HashMap::new()),
                next_request: AtomicU64::new(0),
                generation: AtomicU64::new(0),
                up: watch::channel(false).0,
            }),
        }
    }

    pub fn is_up(&self) -> bool {
        *self.inner.up.borrow()
    }

    pub fn up_watch(&self) -> watch::Receiver<bool> {
        self.inner.up.subscribe()
    }

    pub fn pending_calls(&self) -> Vec<String> {
        crate::lock(&self.inner.shared).outbox.pending_calls().unwrap_or_default()
    }

    pub fn forget(&self, call_id: &str) -> io::Result<()> {
        crate::lock(&self.inner.shared).outbox.forget(call_id)
    }

    /// Stores the frame durably and sends it if the link is up. It is resent
    /// on every reconnect until acknowledged. Never call while holding a lock
    /// the handler's `apply` takes.
    pub fn send_call(&self, call_id: &str, body: CallBody) -> io::Result<u64> {
        let mut sh = crate::lock(&self.inner.shared);
        let seq = sh.outbox.append(call_id, &body)?;
        if let Some(tx) = &sh.current {
            let _ = tx.send(Frame::Call { call_id: call_id.to_string(), dir: self.inner.out_dir, seq, body });
        }
        Ok(seq)
    }

    pub async fn request(&self, body: Request) -> Result<Reply, Refusal> {
        let down = || Refusal::new(RefusalCode::LinkDown, "the voice link is down");
        let Some(tx) = crate::lock(&self.inner.shared).current.clone() else {
            return Err(down());
        };
        let id = self.inner.next_request.fetch_add(1, SeqCst) + 1;
        let (reply_tx, reply_rx) = oneshot::channel();
        crate::lock(&self.inner.pending).insert(id, reply_tx);
        if tx.send(Frame::Request { id, body }).is_err() {
            crate::lock(&self.inner.pending).remove(&id);
            return Err(down());
        }
        match tokio::time::timeout(self.inner.cfg.request_timeout, reply_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(down()),
            Err(_) => {
                crate::lock(&self.inner.pending).remove(&id);
                Err(Refusal::new(RefusalCode::Timeout, "no answer in time"))
            }
        }
    }

    /// Serves one connection until it drops and says why.
    pub async fn serve(&self, stream: UnixStream) -> Disconnect {
        let generation = self.inner.generation.fetch_add(1, SeqCst) + 1;
        let (mut rd, mut wr) = stream.into_split();
        let hello = Frame::Hello {
            proto: PROTO_VERSION,
            role: self.inner.cfg.role,
            instance: self.inner.instance.clone(),
        };
        if let Err(e) = write_frame(&mut wr, &hello).await {
            return Disconnect::Codec(e.to_string());
        }
        match tokio::time::timeout(self.inner.cfg.hello_timeout, read_frame(&mut rd)).await {
            Err(_) => return Disconnect::HelloRefused("no hello in time".into()),
            Ok(Ok(Some(Frame::Hello { proto, role, .. }))) => {
                if proto != PROTO_VERSION {
                    return Disconnect::HelloRefused(format!(
                        "peer speaks protocol {proto}, this side {PROTO_VERSION}"
                    ));
                }
                if role == self.inner.cfg.role {
                    return Disconnect::HelloRefused("peer claims this side's role".into());
                }
            }
            Ok(Ok(Some(other))) => return Disconnect::Protocol(format!("expected hello, got {other:?}")),
            Ok(Ok(None)) => return Disconnect::Eof,
            Ok(Err(e)) => return Disconnect::Codec(e.to_string()),
        }

        let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
        // Replay and install under one lock, so a frame appended meanwhile is
        // neither missed nor sent ahead of an older one.
        {
            let mut sh = crate::lock(&self.inner.shared);
            let calls = match sh.outbox.pending_calls() {
                Ok(c) => c,
                Err(e) => return Disconnect::Protocol(format!("outbox unreadable: {e}")),
            };
            for call_id in calls {
                match sh.outbox.unacked(&call_id, 0) {
                    Ok(frames) => {
                        for (seq, body) in frames {
                            let _ = tx.send(Frame::Call {
                                call_id: call_id.clone(),
                                dir: self.inner.out_dir,
                                seq,
                                body,
                            });
                        }
                    }
                    Err(e) => return Disconnect::Protocol(format!("outbox unreadable: {e}")),
                }
            }
            sh.current = Some(tx.clone());
        }
        self.inner.up.send_replace(true);
        self.inner.handler.link_changed(true);
        // Guards, not trailing statements: a serve future dropped by an abort
        // still closes its socket and marks the link down.
        let _teardown = Teardown { inner: self.inner.clone(), generation };

        let _writer = AbortOnDrop(Some(tokio::spawn(async move {
            while let Some(f) = rx.recv().await {
                if write_frame(&mut wr, &f).await.is_err() {
                    break;
                }
            }
        })));
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Result<Option<Frame>, CodecError>>();
        let _reader = AbortOnDrop(Some(tokio::spawn(async move {
            loop {
                let got = read_frame(&mut rd).await;
                let stop = !matches!(got, Ok(Some(_)));
                if in_tx.send(got).is_err() || stop {
                    break;
                }
            }
        })));

        self.run(&mut in_rx, &tx).await
    }

    async fn run(
        &self,
        incoming: &mut mpsc::UnboundedReceiver<Result<Option<Frame>, CodecError>>,
        tx: &mpsc::UnboundedSender<Frame>,
    ) -> Disconnect {
        let mut tick = tokio::time::interval(self.inner.cfg.heartbeat);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let (mut pinged, mut ponged) = (0u64, 0u64);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if pinged - ponged >= self.inner.cfg.missed_pongs as u64 {
                        return Disconnect::HeartbeatLost;
                    }
                    pinged += 1;
                    let _ = tx.send(Frame::Ping { n: pinged });
                }
                got = incoming.recv() => match got {
                    None | Some(Ok(None)) => return Disconnect::Eof,
                    Some(Err(e)) => return Disconnect::Codec(e.to_string()),
                    Some(Ok(Some(frame))) => {
                        if let Frame::Pong { n } = frame {
                            ponged = ponged.max(n);
                            continue;
                        }
                        if let Err(d) = self.on_frame(frame, tx).await {
                            return d;
                        }
                    }
                }
            }
        }
    }

    async fn on_frame(&self, frame: Frame, tx: &mpsc::UnboundedSender<Frame>) -> Result<(), Disconnect> {
        let wrong_way = |what: &str| Disconnect::Protocol(format!("{what} travelling the wrong way"));
        match frame {
            Frame::Ping { n } => {
                let _ = tx.send(Frame::Pong { n });
            }
            Frame::Pong { .. } => {}
            Frame::Hello { .. } => return Err(Disconnect::Protocol("a second hello".into())),
            Frame::Request { id, body } => {
                let handler = self.inner.handler.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let result = handler.request(body).await;
                    let _ = tx.send(Frame::Response { id, result });
                });
            }
            Frame::Response { id, result } => {
                if let Some(waiter) = crate::lock(&self.inner.pending).remove(&id) {
                    let _ = waiter.send(result);
                }
            }
            Frame::Call { call_id, dir, seq, body } => {
                if dir == self.inner.out_dir {
                    return Err(wrong_way("a call frame"));
                }
                if !valid_call_id(&call_id) {
                    return Err(Disconnect::Protocol(format!("bad call id {call_id:?}")));
                }
                let handler = self.inner.handler.clone();
                let id = call_id.clone();
                let applied = tokio::task::spawn_blocking(move || handler.applied(&id))
                    .await
                    .map_err(|e| Disconnect::Protocol(e.to_string()))?;
                match classify(applied, seq) {
                    Arrival::Apply => {
                        let handler = self.inner.handler.clone();
                        let id = call_id.clone();
                        tokio::task::spawn_blocking(move || handler.apply(&id, seq, body))
                            .await
                            .map_err(|e| Disconnect::Protocol(e.to_string()))?
                            .map_err(|e| Disconnect::Protocol(format!("applying {call_id}#{seq}: {e}")))?;
                        let _ = tx.send(Frame::Ack { call_id, dir, seq });
                    }
                    Arrival::Duplicate => {
                        let _ = tx.send(Frame::Ack { call_id, dir, seq: applied });
                    }
                    Arrival::Gap => {
                        let _ = tx.send(Frame::Resume { call_id, dir, after: applied });
                    }
                }
            }
            Frame::Ack { call_id, dir, seq } => {
                if dir != self.inner.out_dir {
                    return Err(wrong_way("an ack"));
                }
                crate::lock(&self.inner.shared)
                    .outbox
                    .ack(&call_id, seq)
                    .map_err(|e| Disconnect::Protocol(format!("outbox: {e}")))?;
                self.inner.handler.acked(&call_id, seq);
            }
            Frame::Resume { call_id, dir, after } => {
                if dir != self.inner.out_dir {
                    return Err(wrong_way("a resume"));
                }
                let sh = crate::lock(&self.inner.shared);
                let frames = sh
                    .outbox
                    .unacked(&call_id, after)
                    .map_err(|e| Disconnect::Protocol(format!("outbox: {e}")))?;
                for (seq, body) in frames {
                    let _ = tx.send(Frame::Call { call_id: call_id.clone(), dir, seq, body });
                }
            }
        }
        Ok(())
    }
}

/// The voice side: connects, serves, and reconnects forever with jittered
/// backoff capped at 2 s.
pub async fn dial_forever(peer: Peer, path: PathBuf) {
    let mut delay = Duration::from_millis(100);
    loop {
        match UnixStream::connect(&path).await {
            Ok(stream) => {
                delay = Duration::from_millis(100);
                let why = peer.serve(stream).await;
                eprintln!("voice link dropped: {why:?}");
            }
            Err(e) => eprintln!("voice link: cannot reach {}: {e}", path.display()),
        }
        let jitter = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_millis() % 100)
            .unwrap_or(0);
        tokio::time::sleep(delay + Duration::from_millis(jitter as u64)).await;
        delay = (delay * 2).min(Duration::from_secs(2));
    }
}

struct AbortOnDrop(Option<tokio::task::JoinHandle<()>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            h.abort();
        }
    }
}

/// Marks the link down when its connection ends, unless a newer connection
/// has already taken over.
struct Teardown {
    inner: Arc<Inner>,
    generation: u64,
}

impl Drop for Teardown {
    fn drop(&mut self) {
        if self.inner.generation.load(SeqCst) != self.generation {
            return;
        }
        crate::lock(&self.inner.shared).current = None;
        self.inner.up.send_replace(false);
        self.inner.handler.link_changed(false);
        for (_, waiter) in crate::lock(&self.inner.pending).drain() {
            let _ = waiter.send(Err(Refusal::new(RefusalCode::LinkDown, "the voice link dropped")));
        }
    }
}

/// Note's side: a new connection replaces the one before it.
pub async fn listen_forever(peer: Peer, listener: UnixListener) {
    let mut current = AbortOnDrop(None);
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                if let Some(old) = current.0.take() {
                    old.abort();
                }
                let p = peer.clone();
                current.0 = Some(tokio::spawn(async move {
                    let why = p.serve(stream).await;
                    eprintln!("voice link dropped: {why:?}");
                }));
            }
            Err(e) => {
                eprintln!("voice link: accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}
```

In `voice-proto/src/lib.rs`, add:

```rust
pub mod peer;
pub mod testkit;

pub use peer::{dial_forever, listen_forever, BoxFuture, Disconnect, Handler, Peer, PeerConfig};
```

- [ ] **Step 5: Run the suite**

Run: `cargo test -p note-voice-proto`
Expected: all pass. Run it five times in a row (`for i in 1 2 3 4 5; do cargo test -q -p note-voice-proto || break; done`); every run must pass. A flaky timing is a bug to fix, not to retry past.

- [ ] **Step 6: Commit**

```bash
git add voice-proto
git commit -m "feat(voice-proto): a peer that acknowledges, replays and deduplicates call frames, proven against a fault-injecting proxy"
```

---

### Task 4: The voice side's durable journal

**Files:**
- Create: `voice-proto/src/journal.rs`
- Modify: `voice-proto/src/lib.rs`

**Interfaces:**
- Consumes: `Outbox`, `CallBody`, `valid_call_id`.
- Produces:
  - `FileOutbox::open(dir: &Path) -> io::Result<FileOutbox>`, implementing `Outbox`.
  - `AppliedFile::new(dir: &Path) -> AppliedFile`, with `applied(&self, call_id) -> u64`, `set_applied(&self, call_id, seq) -> io::Result<()>` and `forget(&self, call_id) -> io::Result<()>`.

- [ ] **Step 1: Write the failing tests**

In `voice-proto/src/journal.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn frames_survive_a_reopen_and_numbering_continues() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut o = FileOutbox::open(dir.path()).unwrap();
            assert_eq!(o.append("c1", &CallBody::Ringing).unwrap(), 1);
            assert_eq!(o.append("c1", &CallBody::Ended).unwrap(), 2);
            o.ack("c1", 1).unwrap();
        }
        let mut o = FileOutbox::open(dir.path()).unwrap();
        assert_eq!(o.unacked("c1", 0).unwrap(), vec![(2, CallBody::Ended)]);
        assert_eq!(o.pending_calls().unwrap(), vec!["c1".to_string()]);
        assert_eq!(o.append("c1", &CallBody::Ringing).unwrap(), 3);
    }

    #[test]
    fn a_torn_last_line_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut o = FileOutbox::open(dir.path()).unwrap();
            o.append("c1", &CallBody::Ringing).unwrap();
        }
        let path = dir.path().join("c1.out.ndjson");
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{\"seq\":2,\"bo").unwrap();
        let o = FileOutbox::open(dir.path()).unwrap();
        assert_eq!(o.unacked("c1", 0).unwrap(), vec![(1, CallBody::Ringing)]);
    }

    #[test]
    fn forget_removes_the_journal() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = FileOutbox::open(dir.path()).unwrap();
        o.append("c1", &CallBody::Ringing).unwrap();
        o.forget("c1").unwrap();
        assert!(!dir.path().join("c1.out.ndjson").exists());
        assert!(FileOutbox::open(dir.path()).unwrap().pending_calls().unwrap().is_empty());
    }

    #[test]
    fn applied_is_durable_and_forgettable() {
        let dir = tempfile::tempdir().unwrap();
        let a = AppliedFile::new(dir.path());
        assert_eq!(a.applied("c1"), 0);
        a.set_applied("c1", 4).unwrap();
        assert_eq!(AppliedFile::new(dir.path()).applied("c1"), 4);
        a.forget("c1").unwrap();
        assert_eq!(a.applied("c1"), 0);
    }

    #[test]
    fn an_unsafe_call_id_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = FileOutbox::open(dir.path()).unwrap();
        assert!(o.append("../x", &CallBody::Ringing).is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p note-voice-proto journal`
Expected: compile errors.

- [ ] **Step 3: Implement**

`voice-proto/src/journal.rs` (above the tests):

```rust
use crate::frame::{valid_call_id, CallBody};
use crate::stream::Outbox;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Line {
    Frame { seq: u64, body: CallBody },
    Ack { ack: u64 },
}

#[derive(Default)]
struct Journal {
    last: u64,
    frames: BTreeMap<u64, CallBody>,
}

/// One append-only file per call, `<call_id>.out.ndjson`, synced on every
/// write.
pub struct FileOutbox {
    dir: PathBuf,
    calls: HashMap<String, Journal>,
}

fn bad_id(id: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("unsafe call id {id:?}"))
}

impl FileOutbox {
    pub fn open(dir: &Path) -> io::Result<FileOutbox> {
        std::fs::create_dir_all(dir)?;
        let mut calls = HashMap::new();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            let Some(id) = name.strip_suffix(".out.ndjson") else { continue };
            if !valid_call_id(id) {
                continue;
            }
            let mut j = Journal::default();
            for line in io::BufReader::new(std::fs::File::open(&path)?).lines() {
                match serde_json::from_str::<Line>(&line?) {
                    Ok(Line::Frame { seq, body }) => {
                        j.last = j.last.max(seq);
                        j.frames.insert(seq, body);
                    }
                    Ok(Line::Ack { ack }) => j.frames.retain(|s, _| *s > ack),
                    Err(_) => break,
                }
            }
            calls.insert(id.to_string(), j);
        }
        Ok(FileOutbox { dir: dir.to_path_buf(), calls })
    }

    fn path(&self, call_id: &str) -> PathBuf {
        self.dir.join(format!("{call_id}.out.ndjson"))
    }

    fn write_line(&self, call_id: &str, line: &Line) -> io::Result<()> {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.path(call_id))?;
        let mut bytes = serde_json::to_vec(line)?;
        bytes.push(b'\n');
        f.write_all(&bytes)?;
        f.sync_data()
    }
}

impl Outbox for FileOutbox {
    fn append(&mut self, call_id: &str, body: &CallBody) -> io::Result<u64> {
        if !valid_call_id(call_id) {
            return Err(bad_id(call_id));
        }
        let seq = self.calls.get(call_id).map_or(0, |j| j.last) + 1;
        self.write_line(call_id, &Line::Frame { seq, body: body.clone() })?;
        let j = self.calls.entry(call_id.to_string()).or_default();
        j.last = seq;
        j.frames.insert(seq, body.clone());
        Ok(seq)
    }

    fn unacked(&self, call_id: &str, after: u64) -> io::Result<Vec<(u64, CallBody)>> {
        Ok(self
            .calls
            .get(call_id)
            .map(|j| j.frames.range(after + 1..).map(|(s, b)| (*s, b.clone())).collect())
            .unwrap_or_default())
    }

    fn ack(&mut self, call_id: &str, upto: u64) -> io::Result<()> {
        if !self.calls.contains_key(call_id) {
            return Ok(());
        }
        self.write_line(call_id, &Line::Ack { ack: upto })?;
        if let Some(j) = self.calls.get_mut(call_id) {
            j.frames.retain(|s, _| *s > upto);
        }
        Ok(())
    }

    fn pending_calls(&self) -> io::Result<Vec<String>> {
        let mut ids: Vec<String> =
            self.calls.iter().filter(|(_, j)| !j.frames.is_empty()).map(|(id, _)| id.clone()).collect();
        ids.sort();
        Ok(ids)
    }

    fn forget(&mut self, call_id: &str) -> io::Result<()> {
        self.calls.remove(call_id);
        match std::fs::remove_file(self.path(call_id)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// The last applied seq of each call Note sent, `<call_id>.in`, replaced
/// atomically.
pub struct AppliedFile {
    dir: PathBuf,
}

impl AppliedFile {
    pub fn new(dir: &Path) -> AppliedFile {
        AppliedFile { dir: dir.to_path_buf() }
    }

    fn path(&self, call_id: &str) -> PathBuf {
        self.dir.join(format!("{call_id}.in"))
    }

    pub fn applied(&self, call_id: &str) -> u64 {
        if !valid_call_id(call_id) {
            return 0;
        }
        std::fs::read_to_string(self.path(call_id))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn set_applied(&self, call_id: &str, seq: u64) -> io::Result<()> {
        if !valid_call_id(call_id) {
            return Err(bad_id(call_id));
        }
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!("{call_id}.in.tmp"));
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(seq.to_string().as_bytes())?;
        f.sync_data()?;
        std::fs::rename(&tmp, self.path(call_id))?;
        std::fs::File::open(&self.dir)?.sync_all()
    }

    pub fn forget(&self, call_id: &str) -> io::Result<()> {
        match std::fs::remove_file(self.path(call_id)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}
```

In `voice-proto/src/lib.rs`, add `pub mod journal;` and `pub use journal::{AppliedFile, FileOutbox};`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p note-voice-proto`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add voice-proto
git commit -m "feat(voice-proto): the voice side journals its frames and applied seqs to disk"
```

---

### Task 5: Note's schema and SQLite outbox

**Files:**
- Modify: `server/Cargo.toml` (add `note-voice-proto = { path = "../voice-proto" }`)
- Modify: `server/src/db.rs` (v42 plus its test)
- Create: `server/src/voice/mod.rs` (for now, only `pub mod outbox; pub mod links;`), `server/src/voice/outbox.rs`, `server/src/voice/links.rs`
- Modify: `server/src/lib.rs` (`pub mod voice;`)

**Interfaces:**
- Produces:
  - `SqliteOutbox::new(db: Arc<Mutex<Connection>>)`, implementing `Outbox`; `append` requires the `voice_calls` row to exist.
  - `links::{Link, get(conn, user_id) -> Result<Option<Link>>, begin(conn, user_id, mxid, now) -> Result<i64>, set_room(conn, link_id, room_id) -> Result<()>, mark_joined(conn, link_id, room_id, now) -> Result<bool>, remove(conn, user_id) -> Result<bool>, ringable(conn, user_id) -> Result<Option<Link>>}`.
  - `Link { id: i64, mxid: String, room_id: Option<String>, state: String }`.

- [ ] **Step 1: Write the failing migration test**

Append to `server/src/db.rs` tests:

```rust
    #[test]
    fn v42_adds_the_voice_tables() {
        let conn = open_memory().unwrap();
        conn.execute("INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO voice_links (user_id, mxid, state, created_at) VALUES (1, '@a:t', 'invited', 'x')",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO voice_links (user_id, mxid, state, created_at) VALUES (1, '@b:t', 'invited', 'x')",
                [],
            )
            .is_err(),
            "one link per user"
        );
        conn.execute(
            "INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at)
             VALUES ('c1', 1, 'outbound', 'starting', 'x', 'x')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO voice_frames (call_id, seq, body) VALUES ('c1', 1, '{}')", []).unwrap();
        conn.execute(
            "INSERT INTO voice_ops (call_id, op_key, result, created_at) VALUES ('c1', '1:0', '{}', 'x')",
            [],
        )
        .unwrap();
        assert!(conn
            .execute("UPDATE voice_calls SET state = 'dialing' WHERE id = 'c1'", [])
            .is_err());
        conn.execute("DELETE FROM voice_calls WHERE id = 'c1'", []).unwrap();
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM voice_frames", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "frames go with their call");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p note-server --lib v42`
Expected: FAIL, no such table `voice_links`.

- [ ] **Step 3: Add v42**

Append to `MIGRATIONS` after v41:

```rust
    // v42
    "
    CREATE TABLE voice_links (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        user_id INTEGER NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
        mxid TEXT NOT NULL,
        room_id TEXT,
        state TEXT NOT NULL CHECK (state IN ('invited','linked')),
        created_at TEXT NOT NULL,
        linked_at TEXT
    );
    CREATE TABLE voice_calls (
        id TEXT PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        direction TEXT NOT NULL CHECK (direction IN ('outbound','inbound')),
        message TEXT,
        state TEXT NOT NULL CHECK (state IN ('starting','ringing','answered','ended')),
        outcome TEXT,
        ring_by TEXT NOT NULL,
        sent_seq INTEGER NOT NULL DEFAULT 0,
        applied_seq INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        ended_at TEXT
    );
    CREATE INDEX idx_voice_calls_open ON voice_calls(state) WHERE state != 'ended';
    CREATE TABLE voice_frames (
        call_id TEXT NOT NULL REFERENCES voice_calls(id) ON DELETE CASCADE,
        seq INTEGER NOT NULL,
        body TEXT NOT NULL,
        PRIMARY KEY (call_id, seq)
    );
    CREATE TABLE voice_ops (
        call_id TEXT NOT NULL REFERENCES voice_calls(id) ON DELETE CASCADE,
        op_key TEXT NOT NULL,
        result TEXT NOT NULL,
        created_at TEXT NOT NULL,
        PRIMARY KEY (call_id, op_key)
    );
    ",
```

- [ ] **Step 4: Write the failing outbox and link tests**

`server/src/voice/outbox.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use note_voice_proto::{CallBody, Outbox};

    fn db() -> Arc<Mutex<Connection>> {
        let conn = crate::db::open_memory().unwrap();
        conn.execute_batch(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member');
             INSERT INTO voice_calls (id, user_id, direction, state, ring_by, created_at)
                 VALUES ('c1', 1, 'outbound', 'starting', 'x', 'x');",
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[test]
    fn append_ack_and_replay() {
        let mut o = SqliteOutbox::new(db());
        assert_eq!(o.append("c1", &CallBody::HangUp).unwrap(), 1);
        assert_eq!(o.append("c1", &CallBody::HangUp).unwrap(), 2);
        o.ack("c1", 1).unwrap();
        assert_eq!(o.unacked("c1", 0).unwrap(), vec![(2, CallBody::HangUp)]);
        assert_eq!(o.pending_calls().unwrap(), vec!["c1".to_string()]);
        o.ack("c1", 2).unwrap();
        assert!(o.pending_calls().unwrap().is_empty());
        assert_eq!(o.append("c1", &CallBody::HangUp).unwrap(), 3, "sent_seq keeps counting");
    }

    #[test]
    fn append_needs_the_call_row() {
        let mut o = SqliteOutbox::new(db());
        assert!(o.append("nope", &CallBody::HangUp).is_err());
    }
}
```

`server/src/voice/links.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = crate::db::open_memory().unwrap();
        c.execute("INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')", [])
            .unwrap();
        c
    }

    #[test]
    fn a_link_is_ringable_only_once_joined() {
        let c = conn();
        let now = jiff::Timestamp::now();
        let id = begin(&c, 1, "@aki:t", now).unwrap();
        set_room(&c, id, "!r:t").unwrap();
        assert!(ringable(&c, 1).unwrap().is_none(), "invited is not linked");
        assert!(mark_joined(&c, id, "!r:t", now).unwrap());
        let l = ringable(&c, 1).unwrap().unwrap();
        assert_eq!((l.mxid.as_str(), l.room_id.as_deref()), ("@aki:t", Some("!r:t")));
    }

    #[test]
    fn relinking_replaces_the_old_account_and_resets_the_state() {
        let c = conn();
        let now = jiff::Timestamp::now();
        let first = begin(&c, 1, "@aki:t", now).unwrap();
        mark_joined(&c, first, "!r:t", now).unwrap();
        let second = begin(&c, 1, "@other:t", now).unwrap();
        assert_eq!(first, second, "one row per user, same id");
        let l = get(&c, 1).unwrap().unwrap();
        assert_eq!((l.mxid.as_str(), l.state.as_str(), l.room_id), ("@other:t", "invited", None));
        assert!(!mark_joined(&c, 999, "!r:t", now).unwrap(), "an unknown link changes nothing");
        assert!(remove(&c, 1).unwrap());
        assert!(get(&c, 1).unwrap().is_none());
    }
}
```

- [ ] **Step 5: Run to verify it fails**

Run: `cargo test -p note-server --lib voice`
Expected: compile errors.

- [ ] **Step 6: Implement**

`server/src/voice/outbox.rs` (above the tests):

```rust
use note_voice_proto::{CallBody, Outbox};
use rusqlite::Connection;
use std::io;
use std::sync::{Arc, Mutex};

/// Note's outbound call frames, kept in `voice_frames` until acknowledged.
pub struct SqliteOutbox {
    db: Arc<Mutex<Connection>>,
}

impl SqliteOutbox {
    pub fn new(db: Arc<Mutex<Connection>>) -> Self {
        Self { db }
    }
}

fn io_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl Outbox for SqliteOutbox {
    fn append(&mut self, call_id: &str, body: &CallBody) -> io::Result<u64> {
        let conn = crate::db_guard(&self.db);
        let tx = conn.unchecked_transaction().map_err(io_err)?;
        let seq: i64 = tx
            .query_row(
                "UPDATE voice_calls SET sent_seq = sent_seq + 1 WHERE id = ?1 RETURNING sent_seq",
                [call_id],
                |r| r.get(0),
            )
            .map_err(io_err)?;
        tx.execute(
            "INSERT INTO voice_frames (call_id, seq, body) VALUES (?1, ?2, ?3)",
            (call_id, seq, serde_json::to_string(body)?),
        )
        .map_err(io_err)?;
        tx.commit().map_err(io_err)?;
        Ok(seq as u64)
    }

    fn unacked(&self, call_id: &str, after: u64) -> io::Result<Vec<(u64, CallBody)>> {
        let conn = crate::db_guard(&self.db);
        let mut stmt = conn
            .prepare("SELECT seq, body FROM voice_frames WHERE call_id = ?1 AND seq > ?2 ORDER BY seq")
            .map_err(io_err)?;
        let rows = stmt
            .query_map((call_id, after as i64), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(io_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, body) = row.map_err(io_err)?;
            out.push((seq as u64, serde_json::from_str(&body)?));
        }
        Ok(out)
    }

    fn ack(&mut self, call_id: &str, upto: u64) -> io::Result<()> {
        crate::db_guard(&self.db)
            .execute("DELETE FROM voice_frames WHERE call_id = ?1 AND seq <= ?2", (call_id, upto as i64))
            .map_err(io_err)?;
        Ok(())
    }

    fn pending_calls(&self) -> io::Result<Vec<String>> {
        let conn = crate::db_guard(&self.db);
        let mut stmt = conn
            .prepare("SELECT DISTINCT call_id FROM voice_frames ORDER BY call_id")
            .map_err(io_err)?;
        let ids = stmt.query_map([], |r| r.get(0)).map_err(io_err)?;
        ids.collect::<Result<Vec<String>, _>>().map_err(io_err)
    }

    fn forget(&mut self, call_id: &str) -> io::Result<()> {
        crate::db_guard(&self.db)
            .execute("DELETE FROM voice_frames WHERE call_id = ?1", [call_id])
            .map_err(io_err)?;
        Ok(())
    }
}
```

`server/src/voice/links.rs` (above the tests):

```rust
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub id: i64,
    pub mxid: String,
    pub room_id: Option<String>,
    pub state: String,
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Link> {
    Ok(Link { id: r.get(0)?, mxid: r.get(1)?, room_id: r.get(2)?, state: r.get(3)? })
}

pub fn get(conn: &Connection, user_id: i64) -> Result<Option<Link>> {
    Ok(conn
        .query_row("SELECT id, mxid, room_id, state FROM voice_links WHERE user_id = ?1", [user_id], row)
        .optional()?)
}

/// A joined link with its room: the only kind a call may use.
pub fn ringable(conn: &Connection, user_id: i64) -> Result<Option<Link>> {
    Ok(get(conn, user_id)?.filter(|l| l.state == "linked" && l.room_id.is_some()))
}

/// Starts, or restarts, the user's one link, keeping its id.
pub fn begin(conn: &Connection, user_id: i64, mxid: &str, now: jiff::Timestamp) -> Result<i64> {
    Ok(conn.query_row(
        "INSERT INTO voice_links (user_id, mxid, state, created_at) VALUES (?1, ?2, 'invited', ?3)
         ON CONFLICT(user_id) DO UPDATE SET
             mxid = excluded.mxid, state = 'invited', room_id = NULL, linked_at = NULL,
             created_at = excluded.created_at
         RETURNING id",
        (user_id, mxid, now.to_string()),
        |r| r.get(0),
    )?)
}

pub fn set_room(conn: &Connection, link_id: i64, room_id: &str) -> Result<()> {
    conn.execute("UPDATE voice_links SET room_id = ?2 WHERE id = ?1", (link_id, room_id))?;
    Ok(())
}

pub fn mark_joined(conn: &Connection, link_id: i64, room_id: &str, now: jiff::Timestamp) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE voice_links SET state = 'linked', room_id = ?2, linked_at = COALESCE(linked_at, ?3)
         WHERE id = ?1",
        (link_id, room_id, now.to_string()),
    )? > 0)
}

pub fn remove(conn: &Connection, user_id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM voice_links WHERE user_id = ?1", [user_id])? > 0)
}
```

`server/src/voice/mod.rs`:

```rust
pub mod links;
pub mod outbox;
```

- [ ] **Step 7: Run the tests**

Run: `cargo test -p note-server --lib`
Expected: all pass, including `migrations_apply_and_are_idempotent`.

- [ ] **Step 8: Commit**

```bash
git add Cargo.lock server
git commit -m "feat(server): voice links, calls and frames in the database, with an outbox over them"
```

---

### Task 6: Note's voice link — handler, listener, fallthrough, sweep

**Files:**
- Modify: `server/src/voice/mod.rs`
- Modify: `server/src/channels/mod.rs` (serde derives; `pub mod voice;` comes in Task 7)
- Modify: `server/src/config.rs` (`VoiceConfig`, `ServerConfig.voice`)

**Interfaces:**
- Consumes:
  - `Peer`, `Handler`, `listen_forever` (Task 3),
  - `SqliteOutbox` and `links` (Task 5),
  - `deliver_via(db, ladder, user_id, username, msg)` (existing, `channels/mod.rs`).
- Produces:
  - `Voice::new(db: Arc<Mutex<Connection>>) -> Arc<Voice>` and `Voice::with_config(db, cfg: PeerConfig) -> Arc<Voice>`.
  - `Voice::listen(self: &Arc<Self>, path: &Path) -> io::Result<tokio::task::JoinHandle<()>>` (binds, chmods 0660, spawns the listener; aborting the handle closes the live connection).
  - `Voice::spawn_sweeper(self: &Arc<Self>)`.
  - `Voice::set_fallback(&self, ladder: Vec<Arc<dyn Channel>>)`.
  - `Voice::is_up(&self) -> bool`.
  - `Voice::open_dm(&self, link_id: i64, mxid: &str) -> Result<String, Refusal>` (async).
  - `Voice::start_call(&self, user_id: i64, link: &links::Link, msg: &OutboundMessage, now: jiff::Timestamp) -> anyhow::Result<String>`.
  - `Voice::sweep(&self, now: jiff::Timestamp) -> usize`.
  - Constants: `RING_SECS: u32 = 30`, `RING_BY_SECS: i64 = 10`, `STALE_START_SECS: i64 = 20`.
  - `config::VoiceConfig { socket: PathBuf }` and `ServerConfig.voice: Option<VoiceConfig>`.

- [ ] **Step 1: Make messages storable**

In `server/src/channels/mod.rs`:
- Add `use serde::{Deserialize, Serialize};`.
- Derive `Serialize, Deserialize` on `Urgency` (with `#[serde(rename_all = "lowercase")]`), `OutboundMessage` and `Action`.

A stored message is re-delivered after the ring.

- [ ] **Step 2: Config**

In `server/src/config.rs`:

```rust
/// Present turns the voice link on: Note listens on `socket` for the voice
/// service.
#[derive(Debug, Clone, Deserialize)]
pub struct VoiceConfig {
    pub socket: PathBuf,
}
```

and in `ServerConfig`:

```rust
    #[serde(default)]
    pub voice: Option<VoiceConfig>,
```

- [ ] **Step 3: Write the failing unit tests**

In `server/src/voice/mod.rs` tests. These drive the handler directly; the socket path is covered in Task 7.

```rust
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
```

- [ ] **Step 4: Run to verify it fails**

Run: `cargo test -p note-server --lib voice::tests`
Expected: compile errors.

- [ ] **Step 5: Implement**

`server/src/voice/mod.rs`:

```rust
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
```

`Voice.db` and `Voice.handler` stay private. The tests reach them directly because a child `tests` module can see its parent's private fields.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p note-server --lib voice`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add server
git commit -m "feat(server): Note's end of the voice link ends calls once and falls through to the rest of the ladder"
```

---

### Task 7: The voice channel, `ring_for`, and the socket suite

**Files:**
- Create: `server/src/channels/voice.rs`
- Modify: `server/src/channels/mod.rs` (`pub mod voice;`)
- Modify: `server/src/config.rs` (`UserConfig.ring_for`)
- Modify: `server/src/lib.rs` (`AppState.voice`, `with_voice`)
- Modify: `server/src/main.rs`
- Create: `server/tests/voice_link.rs`

**Interfaces:**
- Consumes: `Voice` (Task 6), `links::ringable` (Task 5), and the testkit's `Recording`, `fast` and `eventually` (Task 3).
- Produces:
  - `VoiceChannel::new(voice: Arc<Voice>, db: Arc<Mutex<Connection>>, config_dir: PathBuf)`, named `"voice"`.
  - `channels::voice::rings_for(ring_for: &str, msg: &OutboundMessage) -> bool`.
  - `UserConfig::ring_for(&self) -> &str`.
  - `config::{RING_FOR_URGENT, RING_FOR_NEVER, RING_FOR}`.
  - `AppState.voice: Option<Arc<Voice>>` and `AppState::with_voice(self, Arc<Voice>) -> Self`, which must be called after every other channel is added.

- [ ] **Step 1: Write the failing channel unit tests**

In `server/src/channels/voice.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::Urgency;

    fn msg(urgency: Urgency) -> OutboundMessage {
        OutboundMessage {
            title: "t".into(),
            body: "b".into(),
            urgency,
            event_id: None,
            conversation_id: None,
            actions: Vec::new(),
        }
    }

    #[test]
    fn urgent_rings_only_high() {
        assert!(rings_for("urgent", &msg(Urgency::High)));
        assert!(!rings_for("urgent", &msg(Urgency::Normal)));
        assert!(!rings_for("never", &msg(Urgency::High)));
        assert!(!rings_for("something else", &msg(Urgency::Normal)));
    }
}
```

- [ ] **Step 2: Write the failing socket suite**

`server/tests/voice_link.rs`:

```rust
use note_server::channels::mock::MockChannel;
use note_server::channels::{deliver_via, Channel, OutboundMessage, Urgency};
use note_server::voice::{links, Voice};
use note_server::{auth, db, AppState};
use note_voice_proto::testkit::{eventually, fast, Recording};
use note_voice_proto::{dial_forever, CallBody, Dir, MemOutbox, Outcome, Peer, Role};
use std::sync::{Arc, Mutex};

fn urgent() -> OutboundMessage {
    OutboundMessage {
        title: "Check-in".into(),
        body: "how is the essay going?".into(),
        urgency: Urgency::High,
        event_id: Some(1),
        conversation_id: None,
        actions: Vec::new(),
    }
}

struct Rig {
    dir: tempfile::TempDir,
    state: AppState,
    voice: Arc<Voice>,
    listen: tokio::task::JoinHandle<()>,
    push: Arc<MockChannel>,
    fake: Peer,
    fake_rec: Arc<Recording>,
    _fake_outbox: Arc<Mutex<MemOutbox>>,
}

fn socket(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("voice.sock")
}

async fn rig(linked: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("defaults")).unwrap();
    std::fs::write(
        dir.path().join("defaults/user.toml"),
        "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n",
    )
    .unwrap();
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", false).unwrap();
    if linked {
        let id = links::begin(&conn, 1, "@aki:t", jiff::Timestamp::now()).unwrap();
        links::mark_joined(&conn, id, "!r:t", jiff::Timestamp::now()).unwrap();
    }
    let push = Arc::new(MockChannel::new("push"));
    let state = AppState::new(conn, dir.path().to_path_buf(), dir.path().to_path_buf())
        .with_channels(vec![push.clone()]);
    let voice = Voice::with_config(state.db.clone(), fast(Role::Note));
    let listen = voice.listen(&socket(&dir)).unwrap();
    let state = state.with_voice(voice.clone());
    let fake_rec = Arc::new(Recording::default());
    let fake_outbox: Arc<Mutex<MemOutbox>> = Arc::default();
    let fake = Peer::new(fast(Role::Voice), Dir::ToNote, fake_rec.clone(), Box::new(fake_outbox.clone()));
    tokio::spawn(dial_forever(fake.clone(), socket(&dir)));
    let (v, f) = (voice.clone(), fake.clone());
    eventually("link up", || v.is_up() && f.is_up()).await;
    Rig { dir, state, voice, listen, push, fake, fake_rec, _fake_outbox: fake_outbox }
}

fn deliver(r: &Rig, msg: &OutboundMessage) -> Option<&'static str> {
    deliver_via(&r.state.db, &r.state.channels, 1, "aki", msg)
}

fn started_call(r: &Rig) -> String {
    let seen = r.fake_rec.seen.lock().unwrap();
    let (id, _, body) = seen.first().expect("a Start frame").clone();
    assert!(matches!(body, CallBody::Start { ring_secs: 30, .. }), "{body:?}");
    id
}

#[tokio::test(flavor = "multi_thread")]
async fn an_urgent_message_rings_then_falls_through_on_missed() {
    let r = rig(true).await;
    assert_eq!(r.state.channels[0].name(), "voice");
    assert_eq!(tokio::task::block_in_place(|| deliver(&r, &urgent())), Some("voice"));
    let rec = r.fake_rec.clone();
    eventually("Start reaches the voice side", || !rec.seen.lock().unwrap().is_empty()).await;
    let id = started_call(&r);
    r.fake.send_call(&id, CallBody::Ringing).unwrap();
    r.fake.send_call(&id, CallBody::Outcome { outcome: Outcome::Missed }).unwrap();
    r.fake.send_call(&id, CallBody::Ended).unwrap();
    let push = r.push.clone();
    eventually("the message falls through", || push.seen().len() == 1).await;
    assert_eq!(r.push.seen()[0].1.body, "how is the essay going?");
    let f = r.fake.clone();
    eventually("the voice side's frames are acknowledged", || f.pending_calls().is_empty()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_normal_message_is_not_rung() {
    let r = rig(true).await;
    let mut m = urgent();
    m.urgency = Urgency::Normal;
    assert_eq!(tokio::task::block_in_place(|| deliver(&r, &m)), Some("push"));
    assert!(r.fake_rec.seen.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn invited_but_not_joined_is_not_rung() {
    let r = rig(false).await;
    {
        let conn = r.state.db();
        links::begin(&conn, 1, "@aki:t", jiff::Timestamp::now()).unwrap();
    }
    assert_eq!(tokio::task::block_in_place(|| deliver(&r, &urgent())), Some("push"));
}

#[tokio::test(flavor = "multi_thread")]
async fn down_link_falls_through_at_once() {
    let r = rig(true).await;
    r.listen.abort();
    let v = r.voice.clone();
    eventually("Note sees the link down", || !v.is_up()).await;
    assert_eq!(tokio::task::block_in_place(|| deliver(&r, &urgent())), Some("push"));
    let open: i64 = r.state.db().query_row("SELECT COUNT(*) FROM voice_calls", [], |x| x.get(0)).unwrap();
    assert_eq!(open, 0, "no call is recorded when the link is down");
}

#[tokio::test(flavor = "multi_thread")]
async fn note_restart_mid_ring_applies_the_outcome_once() {
    let r = rig(true).await;
    tokio::task::block_in_place(|| deliver(&r, &urgent()));
    let rec = r.fake_rec.clone();
    eventually("Start arrives", || !rec.seen.lock().unwrap().is_empty()).await;
    let id = started_call(&r);
    r.fake.send_call(&id, CallBody::Ringing).unwrap();

    r.listen.abort();
    let f = r.fake.clone();
    eventually("the voice side sees Note gone", || !f.is_up()).await;
    r.fake.send_call(&id, CallBody::Outcome { outcome: Outcome::Declined }).unwrap();
    r.fake.send_call(&id, CallBody::Ended).unwrap();

    let reborn = Voice::with_config(r.state.db.clone(), fast(Role::Note));
    reborn.set_fallback(vec![r.push.clone()]);
    let _listen = reborn.listen(&socket(&r.dir)).unwrap();
    let f = r.fake.clone();
    eventually("the outbox drains into the new Note", || f.pending_calls().is_empty()).await;
    let push = r.push.clone();
    eventually("one fallthrough", || push.seen().len() == 1).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(r.push.seen().len(), 1);
    let outcome: String = r
        .state
        .db()
        .query_row("SELECT outcome FROM voice_calls WHERE id = ?1", [&id], |x| x.get(0))
        .unwrap();
    assert_eq!(outcome, "declined");
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p note-server --test voice_link`
Expected: compile errors (`with_voice`, `channels::voice` missing).

- [ ] **Step 4: Implement**

`server/src/config.rs`, in `UserConfig` (after `session_end_notify`):

```rust
    /// Which messages ring the linked phone: `urgent` or `never`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring_for: Option<String>,
```

Module level:

```rust
pub const RING_FOR_URGENT: &str = "urgent";
pub const RING_FOR_NEVER: &str = "never";
pub const RING_FOR: &[&str] = &[RING_FOR_URGENT, RING_FOR_NEVER];
```

In `impl UserConfig`:

```rust
    pub fn ring_for(&self) -> &str {
        self.ring_for.as_deref().unwrap_or(RING_FOR_URGENT)
    }
```

`server/src/channels/voice.rs` (above the tests):

```rust
use super::{Channel, OutboundMessage, Urgency};
use crate::voice::{links, Voice};
use anyhow::Context;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub fn rings_for(ring_for: &str, msg: &OutboundMessage) -> bool {
    ring_for == crate::config::RING_FOR_URGENT && msg.urgency == Urgency::High
}

/// First in the ladder: rings the linked phone. `Ok` means the voice side
/// holds the call; the message itself follows through the rest of the
/// ladder once the ring ends.
pub struct VoiceChannel {
    voice: Arc<Voice>,
    db: Arc<Mutex<Connection>>,
    config_dir: PathBuf,
}

impl VoiceChannel {
    pub fn new(voice: Arc<Voice>, db: Arc<Mutex<Connection>>, config_dir: PathBuf) -> Self {
        Self { voice, db, config_dir }
    }
}

impl Channel for VoiceChannel {
    fn name(&self) -> &'static str {
        "voice"
    }

    fn deliver(&self, user_id: i64, username: &str, msg: &OutboundMessage) -> anyhow::Result<()> {
        let cfg = crate::config::UserConfig::load(&self.config_dir, username)?;
        anyhow::ensure!(rings_for(cfg.ring_for(), msg), "not a message this user is rung for");
        let link = {
            let conn = crate::db_guard(&self.db);
            links::ringable(&conn, user_id)?
        }
        .context("no linked Matrix account")?;
        anyhow::ensure!(self.voice.is_up(), "the voice service is not connected");
        self.voice.start_call(user_id, &link, msg, jiff::Timestamp::now())?;
        Ok(())
    }
}
```

In `server/src/channels/mod.rs`, add `pub mod voice;`.

`server/src/lib.rs`: add the field `pub voice: Option<Arc<crate::voice::Voice>>,` to `AppState` (initialized to `None` in `new`), and:

```rust
    /// Must come after every other channel: the ones present now are what a
    /// rung message falls through to.
    pub fn with_voice(mut self, voice: Arc<crate::voice::Voice>) -> Self {
        voice.set_fallback(self.channels.clone());
        let ch = crate::channels::voice::VoiceChannel::new(voice.clone(), self.db.clone(), self.config_dir.clone());
        self.channels.insert(0, Arc::new(ch));
        self.voice = Some(voice);
        self
    }
```

`server/src/main.rs`, after the telegram block and before the embeddings backfill:

```rust
    if let Some(v) = &cfg.voice {
        let voice = note_server::voice::Voice::new(state.db.clone());
        voice
            .listen(&v.socket)
            .with_context(|| format!("listening for the voice service on {}", v.socket.display()))?;
        voice.spawn_sweeper();
        state = state.with_voice(voice);
    }
```

In `config/server.toml` (the tracked sample), under the channels section:

```toml
# The voice service (note-voice) connects here to ring a linked Matrix account.
# [voice]
# socket = "/run/note/voice.sock"
```

- [ ] **Step 5: Run the suites**

Run: `cargo test -p note-server`
Expected: all pass. Run `cargo test -p note-server --test voice_link` five times; every run passes.

- [ ] **Step 6: Commit**

```bash
git add config/server.toml server
git commit -m "feat(server): an urgent message rings the linked phone first and falls through after"
```

---

### Task 8: Settings, linking and a test ring (API + web)

**Files:**
- Modify: `server/src/api.rs`
- Modify: `web/src/types.ts`, `web/src/api.ts`, `web/src/views/Settings.tsx`
- Test: `server/tests/voice_link.rs` (append)

**Interfaces:**
- Consumes: `Voice::open_dm`, `Voice::start_call`, `links::*`, `UserConfig::ring_for`, `config::RING_FOR`.
- Produces:
  - `POST /api/voice/link {mxid}` → `200 {mxid, state: "invited", room_id}`.
  - `DELETE /api/voice/link` → `204`.
  - `POST /api/voice/test` → `202 {call_id}`, or `409` when not linked or the link is down.
  - Settings JSON gains `voice_enabled: bool`, `voice_link: {mxid, state} | null` and `ring_for: "urgent" | "never"`.
  - `SettingsPatch.ring_for`.

- [ ] **Step 1: Write the failing API tests**

Append to `server/tests/voice_link.rs`:

```rust
mod common;

use axum::body::Body;
use axum::http::{header, Request as HttpRequest, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn api_rig() -> (axum::Router, String, Rig) {
    let r = rig(false).await;
    let app = note_server::api::router(r.state.clone());
    let cookie = common::login(&app, "aki", "pw").await;
    (app, cookie, r)
}

async fn call(app: &axum::Router, cookie: &str, method: &str, uri: &str, body: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method(method)
                .uri(uri)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

#[tokio::test(flavor = "multi_thread")]
async fn linking_opens_a_dm_and_settings_show_it() {
    let (app, cookie, r) = api_rig().await;
    r.fake_rec.answer_with(|req| match req {
        note_voice_proto::Request::OpenDm { link_id, .. } => {
            Ok(note_voice_proto::Reply::Dm { room_id: format!("!dm{link_id}:t") })
        }
        _ => Err(note_voice_proto::Refusal::new(note_voice_proto::RefusalCode::BadRequest, "no")),
    });
    let (status, body) = call(&app, &cookie, "POST", "/api/voice/link", r#"{"mxid":"@aki:t"}"#).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "invited");
    let (_, s) = call(&app, &cookie, "GET", "/api/settings", "").await;
    assert_eq!(s["voice_enabled"], true);
    assert_eq!(s["voice_link"]["mxid"], "@aki:t");
    assert_eq!(s["ring_for"], "urgent");

    let (status, _) = call(&app, &cookie, "PUT", "/api/settings", r#"{"ring_for":"never"}"#).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, &cookie, "PUT", "/api/settings", r#"{"ring_for":"sometimes"}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = call(&app, &cookie, "DELETE", "/api/voice/link", "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, s) = call(&app, &cookie, "GET", "/api/settings", "").await;
    assert!(s["voice_link"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_matrix_id_is_refused_before_anything_is_stored() {
    let (app, cookie, r) = api_rig().await;
    for bad in [r#"{"mxid":"aki"}"#, r#"{"mxid":"@aki"}"#, r#"{"mxid":"@a ki:t"}"#] {
        let (status, _) = call(&app, &cookie, "POST", "/api/voice/link", bad).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
    assert!(links::get(&r.state.db(), 1).unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_test_ring_needs_a_joined_link() {
    let (app, cookie, r) = api_rig().await;
    let (status, _) = call(&app, &cookie, "POST", "/api/voice/test", "").await;
    assert_eq!(status, StatusCode::CONFLICT);
    {
        let conn = r.state.db();
        let id = links::begin(&conn, 1, "@aki:t", jiff::Timestamp::now()).unwrap();
        links::mark_joined(&conn, id, "!r:t", jiff::Timestamp::now()).unwrap();
    }
    let (status, body) = call(&app, &cookie, "POST", "/api/voice/test", "").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let rec = r.fake_rec.clone();
    eventually("the test ring starts", || !rec.seen.lock().unwrap().is_empty()).await;
}
```

`api_rig` reuses `rig` and logs in as `aki`/`pw`, the account `rig` created.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p note-server --test voice_link`
Expected: FAIL (404 on `/api/voice/link`).

- [ ] **Step 3: Implement the API**

In `server/src/api.rs`, add routes next to the telegram ones:

```rust
        .route("/api/voice/link", post(voice_link).delete(voice_unlink))
        .route("/api/voice/test", post(voice_test))
```

Add `ring_for: Option<String>,` to `SettingsPatch` and, in `settings_put` before `alerts`:

```rust
    if let Some(ring_for) = req.ring_for {
        if !crate::config::RING_FOR.contains(&ring_for.as_str()) {
            return invalid_field("ring_for", "must be urgent or never");
        }
        cfg.ring_for = Some(ring_for);
    }
```

Give `settings_body` one more parameter, `voice_link: Option<crate::voice::links::Link>`, and add to its JSON:

```rust
        "voice_enabled": state.voice.is_some(),
        "voice_link": voice_link.map(|l| serde_json::json!({ "mxid": l.mxid, "state": l.state })),
        "ring_for": cfg.ring_for(),
```

At both call sites (GET under its own guard, PUT under the held `conn`), read it with `crate::voice::links::get(&conn, user.id).unwrap_or(None)`.

Handlers:

```rust
fn valid_mxid(mxid: &str) -> bool {
    let Some(rest) = mxid.strip_prefix('@') else { return false };
    let Some((local, server)) = rest.split_once(':') else { return false };
    !local.is_empty()
        && !server.is_empty()
        && mxid.len() <= 255
        && !mxid.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[derive(Deserialize)]
struct VoiceLinkReq {
    mxid: String,
}

/// Starts or restarts the link and has the voice service invite the account
/// to a fresh DM; the link turns `linked` when the invite is accepted.
async fn voice_link(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<VoiceLinkReq>,
) -> impl IntoResponse {
    let Some(voice) = state.voice.clone() else {
        return (StatusCode::CONFLICT, Json(serde_json::json!({ "error": "calls are not set up on this server" })))
            .into_response();
    };
    let mxid = req.mxid.trim().to_string();
    if !valid_mxid(&mxid) {
        return invalid_field("mxid", "must look like @name:server");
    }
    let link_id = {
        let conn = state.db();
        match crate::voice::links::begin(&conn, user.id, &mxid, jiff::Timestamp::now()) {
            Ok(id) => id,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    };
    match voice.open_dm(link_id, &mxid).await {
        Ok(room_id) => {
            let conn = state.db();
            if crate::voice::links::set_room(&conn, link_id, &room_id).is_err() {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            Json(serde_json::json!({ "mxid": mxid, "state": "invited", "room_id": room_id })).into_response()
        }
        Err(refusal) => {
            let conn = state.db();
            let _ = crate::log::record(&conn, Some(user.id), "voice_link_error", &refusal.to_string());
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "The call service isn't reachable right now. Try again in a minute." })),
            )
                .into_response()
        }
    }
}

async fn voice_unlink(user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.db();
    match crate::voice::links::remove(&conn, user.id) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Rings the linked phone now, whatever `ring_for` says.
async fn voice_test(user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    let conflict = |msg: &str| (StatusCode::CONFLICT, Json(serde_json::json!({ "error": msg }))).into_response();
    let Some(voice) = state.voice.clone() else {
        return conflict("calls are not set up on this server");
    };
    let link = {
        let conn = state.db();
        crate::voice::links::ringable(&conn, user.id).unwrap_or(None)
    };
    let Some(link) = link else { return conflict("link a Matrix account first") };
    if !voice.is_up() {
        return conflict("the call service isn't connected");
    }
    let msg = crate::channels::OutboundMessage {
        title: "Test call".into(),
        body: "This was a test call from Note.".into(),
        urgency: crate::channels::Urgency::High,
        event_id: None,
        conversation_id: None,
        actions: Vec::new(),
    };
    match voice.start_call(user.id, &link, &msg, jiff::Timestamp::now()) {
        Ok(call_id) => (StatusCode::ACCEPTED, Json(serde_json::json!({ "call_id": call_id }))).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
```

`invalid_field` already returns 422, matching the tests. If the existing helper returns a different status, keep the helper and change the tests' expected status to match it.

- [ ] **Step 4: Run the API tests**

Run: `cargo test -p note-server --test voice_link`
Expected: all pass.

- [ ] **Step 5: Web client**

`web/src/types.ts`, in `Settings`:

```ts
  // Whether this server can ring a phone; the Calls row hides without it.
  voice_enabled: boolean
  voice_link: { mxid: string; state: 'invited' | 'linked' } | null
  ring_for: 'urgent' | 'never'
```

`web/src/api.ts`, next to the telegram calls:

```ts
  voiceLink: (mxid: string) =>
    request<{ mxid: string; state: 'invited'; room_id: string }>('/api/voice/link', {
      method: 'POST',
      body: JSON.stringify({ mxid }),
    }),
  voiceUnlink: () => request<void>('/api/voice/link', { method: 'DELETE' }),
  voiceTest: () => request<{ call_id: string }>('/api/voice/test', { method: 'POST' }),
```

`request` must already set JSON headers for bodies (check `web/src/api.ts`; the telegram and settings calls show the pattern).

`web/src/views/Settings.tsx`:
- Extend `Loaded` with `voiceEnabled: boolean`, `voiceLink: Settings['voice_link']` and `ringFor: 'urgent' | 'never'`, and fill them in `load()` from `s.voice_enabled`, `s.voice_link` and `s.ring_for`.
- Add state `const [mxid, setMxid] = useState('')`.
- Add the handlers:

```tsx
  const linkVoice = async () => {
    setSave({ row: 'calls', kind: 'busy' })
    try {
      const got = await api.voiceLink(mxid.trim())
      setState((s) =>
        s && s !== 'error' ? { ...s, voiceLink: { mxid: got.mxid, state: 'invited' } } : s,
      )
      setSave(null)
    } catch (err) {
      setSave({ row: 'calls', kind: 'failed', message: failure(err) })
    }
  }

  const unlinkVoice = async () => {
    setSave({ row: 'calls', kind: 'busy' })
    try {
      await api.voiceUnlink()
      setState((s) => (s && s !== 'error' ? { ...s, voiceLink: null } : s))
      setSave({ row: 'calls', kind: 'saved' })
    } catch (err) {
      setSave({ row: 'calls', kind: 'failed', message: failure(err) })
    }
  }

  const ringNow = async () => {
    setSave({ row: 'calls', kind: 'busy' })
    try {
      await api.voiceTest()
      setSave({ row: 'calls', kind: 'saved', message: 'Ringing' })
    } catch (err) {
      setSave({ row: 'calls', kind: 'failed', message: failure(err) })
    }
  }

  const saveRingFor = async (ringFor: 'urgent' | 'never') => {
    setSave({ row: 'calls', kind: 'busy' })
    try {
      const saved = await api.saveSettings({ ring_for: ringFor })
      setState((s) => (s && s !== 'error' ? { ...s, ringFor: saved.ring_for } : s))
      setSave({ row: 'calls', kind: 'saved' })
    } catch (err) {
      setSave({ row: 'calls', kind: 'failed', message: failure(err) })
    }
  }

  // The invite is accepted over in Element, so the row watches for it.
  const invited = loaded?.voiceLink?.state === 'invited'
  useEffect(() => {
    if (!invited) return
    const id = window.setInterval(() => {
      void api
        .settings()
        .then((s) => {
          if (s.voice_link?.state !== 'linked') return
          setState((prev) => (prev && prev !== 'error' ? { ...prev, voiceLink: s.voice_link } : prev))
        })
        .catch(() => undefined)
    }, 3000)
    return () => window.clearInterval(id)
  }, [invited])
```

`api.saveSettings` must accept `ring_for`. Widen its patch type in `api.ts` the way `session_end_notify` is accepted.

In the `Notifications` group, after the Telegram row:

```tsx
        {loaded?.voiceEnabled && (
          <FoldRow
            label="Calls"
            value={
              loaded.voiceLink?.state === 'linked'
                ? loaded.ringFor === 'never'
                  ? 'Off'
                  : 'Urgent'
                : loaded.voiceLink
                  ? 'Invited'
                  : 'Not linked'
            }
            open={open === 'calls'}
            onToggle={fold('calls')}
          >
            {open === 'calls' && (
              <div className="set-fold-body">
                {loaded.voiceLink?.state === 'linked' ? (
                  <>
                    <span className="set-sub">{loaded.voiceLink.mxid}</span>
                    <div className="set-seg" role="radiogroup" aria-label="Ring me for">
                      {(['urgent', 'never'] as const).map((v) => (
                        <button
                          key={v}
                          type="button"
                          role="radio"
                          aria-checked={loaded.ringFor === v}
                          className={loaded.ringFor === v ? 'on' : ''}
                          disabled={busy}
                          onClick={() => void saveRingFor(v)}
                        >
                          {v === 'urgent' ? 'Urgent' : 'Never'}
                        </button>
                      ))}
                    </div>
                    <button type="button" className="btn-haze small" disabled={busy} onClick={() => void ringNow()}>
                      Ring me
                    </button>
                    <button type="button" className="btn-haze small" disabled={busy} onClick={() => void unlinkVoice()}>
                      Unlink
                    </button>
                  </>
                ) : loaded.voiceLink ? (
                  <>
                    <span className="set-sub">Accept Note's invite in Element</span>
                    <button type="button" className="btn-haze small" disabled={busy} onClick={() => void unlinkVoice()}>
                      Cancel
                    </button>
                  </>
                ) : (
                  <form
                    className="set-inline"
                    onSubmit={(e) => {
                      e.preventDefault()
                      void linkVoice()
                    }}
                  >
                    <input
                      id="voice-mxid"
                      className="set-input"
                      placeholder="@you:server"
                      autoComplete="off"
                      value={mxid}
                      onChange={(e) => setMxid(e.target.value)}
                    />
                    <button type="submit" className="btn-haze small" disabled={busy || !mxid.trim()}>
                      Link
                    </button>
                  </form>
                )}
                <Status save={save} row="calls" />
              </div>
            )}
          </FoldRow>
        )}
```

Reuse whatever segmented-control, inline-form and input classes `Settings.tsx` and `styles.css` already use; `grep -n "set-seg\|set-input\|set-inline" web/src/styles.css web/src/views/Settings.tsx`. Where one is missing, use the closest existing class instead of inventing a new look. Keep the copy this short (see the "Note UI design taste" memory: as little text as possible).

- [ ] **Step 6: Build and test the web client**

Run: `pnpm -C web build && pnpm -C web test`
Expected: the type check and build pass; the existing tests pass.

- [ ] **Step 7: Commit**

```bash
git add server web
git commit -m "feat: link a Matrix account for calls, choose what rings, and ring on demand from Settings"
```

---

### Task 9: `note-voice` — Matrix client and one outbound ring

**Files:**
- Modify: `Cargo.toml` (members + `"voice"`)
- Create: `voice/Cargo.toml`, `voice/src/lib.rs`, `voice/src/config.rs`, `voice/src/state.rs`, `voice/src/matrix.rs`, `voice/src/calls.rs`, `voice/src/service.rs`, `voice/src/main.rs`
- Create: `voice/tests/common/mod.rs`, `voice/tests/ring.rs`

**Interfaces:**
- Consumes: `note_voice_proto::*` (Tasks 1–4).
- Produces:
  - `config::VoiceServiceConfig { homeserver, token_file, livekit_service_url, socket, state_dir }` and `VoiceServiceConfig::load(path) -> anyhow::Result<Self>`.
  - `Matrix::connect(homeserver: &str, token: &str) -> anyhow::Result<Matrix>`, with:
    - `create_dm(&self, mxid) -> Result<String>`
    - `invite(&self, room, mxid)`
    - `put_member(&self, room, expires_ms, livekit_url) -> Result<String>`
    - `clear_member(&self, room)`
    - `ring(&self, room, target, member_event_id, lifetime_ms) -> Result<String>`
    - `sync(&self, since: Option<&str>, timeout_ms: u64) -> Result<SyncBatch>`
  - `SyncBatch { next_batch: String, events: Vec<RoomEvent> }` and `RoomEvent::{Joined {room, user}, CallMember {room, user, active: bool}, Declined {room, notification}}`.
  - `service::run(cfg: VoiceServiceConfig) -> anyhow::Result<()>` and `service::run_with(cfg, peer_cfg: PeerConfig) -> anyhow::Result<()>`.

- [ ] **Step 1: Manifest**

`voice/Cargo.toml`:

```toml
[package]
name = "note-voice"
version = "0.1.0"
edition = "2021"
license = "Unlicense"

[lib]
name = "note_voice"
path = "src/lib.rs"

[[bin]]
name = "note-voice"
path = "src/main.rs"

[dependencies]
note-voice-proto = { path = "../voice-proto" }
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.13", default-features = false, features = ["json", "rustls", "http2", "query"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "1"
anyhow = "1"
uuid = { version = "1", features = ["v4"] }
urlencoding = "2"

[dev-dependencies]
axum = "0.8"
tempfile = "3"
```

In the root `Cargo.toml`, set `members = ["server", "voice-proto", "voice"]`.

- [ ] **Step 2: Write the mock homeserver and fake Note**

`voice/tests/common/mod.rs`:

```rust
use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use note_voice_proto::testkit::{fast, Recording};
use note_voice_proto::{listen_forever, Dir, MemOutbox, Peer, Role};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Hs {
    pub created: Vec<Value>,
    pub invites: Vec<(String, String)>,
    pub state_puts: Vec<(String, String, String, Value)>,
    /// Room, event type, content, and the event id handed back.
    pub sends: Vec<(String, String, Value, String)>,
    pub syncs: VecDeque<Value>,
    pub fail_sends: bool,
    rooms: u32,
    events: u32,
}

pub type SharedHs = Arc<Mutex<Hs>>;

async fn whoami() -> Json<Value> {
    Json(json!({ "user_id": "@note:t", "device_id": "DEV" }))
}

async fn create_room(State(hs): State<SharedHs>, Json(body): Json<Value>) -> Json<Value> {
    let mut hs = hs.lock().unwrap();
    hs.rooms += 1;
    hs.created.push(body);
    Json(json!({ "room_id": format!("!room{}:t", hs.rooms) }))
}

async fn invite(State(hs): State<SharedHs>, Path(room): Path<String>, Json(body): Json<Value>) -> Json<Value> {
    hs.lock().unwrap().invites.push((room, body["user_id"].as_str().unwrap_or_default().into()));
    Json(json!({}))
}

async fn put_state(
    State(hs): State<SharedHs>,
    Path((room, kind, key)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut hs = hs.lock().unwrap();
    hs.events += 1;
    hs.state_puts.push((room, kind, key, body));
    Json(json!({ "event_id": format!("$s{}", hs.events) }))
}

async fn send(
    State(hs): State<SharedHs>,
    Path((room, kind, _txn)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let mut hs = hs.lock().unwrap();
    if hs.fail_sends {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            Json(json!({ "errcode": "M_UNKNOWN_TOKEN", "error": "revoked" })),
        ));
    }
    hs.events += 1;
    let event_id = format!("$e{}", hs.events);
    hs.sends.push((room, kind, body, event_id.clone()));
    Ok(Json(json!({ "event_id": event_id })))
}

async fn direct() -> (axum::http::StatusCode, Json<Value>) {
    (axum::http::StatusCode::NOT_FOUND, Json(json!({ "errcode": "M_NOT_FOUND" })))
}

async fn set_direct() -> Json<Value> {
    Json(json!({}))
}

async fn sync(State(hs): State<SharedHs>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let n: u64 = q.get("since").and_then(|s| s.trim_start_matches('s').parse().ok()).unwrap_or(0);
    for _ in 0..20 {
        if let Some(mut next) = hs.lock().unwrap().syncs.pop_front() {
            next["next_batch"] = json!(format!("s{}", n + 1));
            return Json(next);
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Json(json!({ "next_batch": format!("s{}", n + 1), "rooms": {} }))
}

pub async fn homeserver() -> (String, SharedHs) {
    let hs: SharedHs = Arc::default();
    let app = Router::new()
        .route("/_matrix/client/v3/account/whoami", get(whoami))
        .route("/_matrix/client/v3/createRoom", post(create_room))
        .route("/_matrix/client/v3/rooms/{room}/invite", post(invite))
        .route("/_matrix/client/v3/rooms/{room}/state/{kind}/{key}", put(put_state))
        .route("/_matrix/client/v3/rooms/{room}/send/{kind}/{txn}", put(send))
        .route("/_matrix/client/v3/user/{user}/account_data/m.direct", get(direct).put(set_direct))
        .route("/_matrix/client/v3/sync", get(sync))
        .with_state(hs.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, hs)
}

pub fn joined_room(room: &str, events: Vec<Value>) -> Value {
    json!({ "rooms": { "join": { room: { "timeline": { "events": events }, "state": { "events": [] } } } } })
}

pub fn member_event(user: &str, active: bool) -> Value {
    let content = if active { json!({ "application": "m.call", "device_id": "PHONE" }) } else { json!({}) };
    json!({
        "type": "org.matrix.msc3401.call.member",
        "state_key": format!("_{user}_PHONE_m.call"),
        "sender": user,
        "content": content,
    })
}

pub fn decline_event(user: &str, notification: &str) -> Value {
    json!({
        "type": "org.matrix.msc4310.rtc.decline",
        "sender": user,
        "content": { "m.relates_to": { "rel_type": "m.reference", "event_id": notification } },
    })
}

pub fn join_event(user: &str) -> Value {
    json!({ "type": "m.room.member", "state_key": user, "sender": user, "content": { "membership": "join" } })
}

pub struct FakeNote {
    pub peer: Peer,
    pub rec: Arc<Recording>,
    pub _outbox: Arc<Mutex<MemOutbox>>,
}

pub fn fake_note(socket: &std::path::Path) -> FakeNote {
    let rec = Arc::new(Recording::default());
    let outbox: Arc<Mutex<MemOutbox>> = Arc::default();
    let peer = Peer::new(fast(Role::Note), Dir::ToVoice, rec.clone(), Box::new(outbox.clone()));
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    tokio::spawn(listen_forever(peer.clone(), listener));
    FakeNote { peer, rec, _outbox: outbox }
}
```

- [ ] **Step 3: Write the failing ring suite**

`voice/tests/ring.rs`:

```rust
mod common;

use common::*;
use note_voice::config::VoiceServiceConfig;
use note_voice_proto::testkit::{eventually, fast};
use note_voice_proto::{CallBody, Outcome, Role};

struct Rig {
    _dir: tempfile::TempDir,
    hs: SharedHs,
    note: FakeNote,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let (base, hs) = homeserver().await;
    let token = dir.path().join("token");
    std::fs::write(&token, "secret\n").unwrap();
    let socket = dir.path().join("voice.sock");
    let note = fake_note(&socket);
    let cfg = VoiceServiceConfig {
        homeserver: base,
        token_file: token,
        livekit_service_url: "https://rtc.t".into(),
        socket,
        state_dir: dir.path().join("state"),
    };
    tokio::spawn(async move { note_voice::service::run_with(cfg, fast(Role::Voice)).await.unwrap() });
    let p = note.peer.clone();
    eventually("voice connects", || p.is_up()).await;
    Rig { _dir: dir, hs, note }
}

fn start(ring_secs: u32, ring_by_ms: i64) -> CallBody {
    CallBody::Start {
        user_id: 1,
        room_id: "!r:t".into(),
        mxid: "@aki:t".into(),
        title: "Check-in".into(),
        ring_secs,
        ring_by_ms,
    }
}

fn soon() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64 + 10_000
}

fn outcome_of(r: &Rig, call: &str) -> Option<Outcome> {
    r.note.rec.bodies(call).into_iter().find_map(|b| match b {
        CallBody::Outcome { outcome } => Some(outcome),
        _ => None,
    })
}

/// The ring's event id, once it is out.
async fn wait_for_ring(r: &Rig) -> String {
    let hs = r.hs.clone();
    eventually("the ring is sent", || {
        hs.lock().unwrap().sends.iter().any(|(_, k, _, _)| k == "m.rtc.notification")
    })
    .await;
    let hs = r.hs.lock().unwrap();
    let (_, _, body, event_id) = hs.sends.iter().find(|(_, k, _, _)| k == "m.rtc.notification").unwrap();
    assert_eq!(body["notification_type"], "ring");
    assert_eq!(body["m.mentions"]["user_ids"][0], "@aki:t");
    assert_eq!(body["m.relates_to"]["rel_type"], "m.reference");
    event_id.clone()
}

fn cleared(r: &Rig) -> bool {
    r.hs.lock().unwrap().state_puts.iter().any(|(_, k, key, body)| {
        k == "org.matrix.msc3401.call.member" && key == "_@note:t_DEV_m.call" && body == &serde_json::json!({})
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_ends_the_ring_as_answered() {
    let r = rig().await;
    r.note.peer.send_call("c1", start(30, soon())).unwrap();
    wait_for_ring(&r).await;
    r.hs.lock().unwrap().syncs.push_back(joined_room("!r:t", vec![member_event("@aki:t", true)]));
    let rec = r.note.rec.clone();
    eventually("an outcome and Ended", || rec.bodies("c1").contains(&CallBody::Ended)).await;
    assert_eq!(outcome_of(&r, "c1"), Some(Outcome::Answered));
    assert_eq!(r.note.rec.bodies("c1")[0], CallBody::Ringing);
    assert!(cleared(&r), "the bot leaves the call");
}

#[tokio::test(flavor = "multi_thread")]
async fn decline_ends_the_ring() {
    let r = rig().await;
    r.note.peer.send_call("c2", start(30, soon())).unwrap();
    let notification = wait_for_ring(&r).await;
    r.hs.lock().unwrap().syncs.push_back(joined_room("!r:t", vec![decline_event("@aki:t", &notification)]));
    let rec = r.note.rec.clone();
    eventually("declined", || rec.bodies("c2").contains(&CallBody::Ended)).await;
    assert_eq!(outcome_of(&r, "c2"), Some(Outcome::Declined));
}

#[tokio::test(flavor = "multi_thread")]
async fn no_answer_is_missed() {
    let r = rig().await;
    r.note.peer.send_call("c3", start(1, soon())).unwrap();
    let rec = r.note.rec.clone();
    eventually("missed", || rec.bodies("c3").contains(&CallBody::Ended)).await;
    assert_eq!(outcome_of(&r, "c3"), Some(Outcome::Missed));
    assert!(cleared(&r));
}

#[tokio::test(flavor = "multi_thread")]
async fn late_start_is_refused_without_touching_matrix() {
    let r = rig().await;
    r.note.peer.send_call("c4", start(30, soon() - 60_000)).unwrap();
    let rec = r.note.rec.clone();
    eventually("refused", || rec.bodies("c4").contains(&CallBody::Ended)).await;
    assert_eq!(outcome_of(&r, "c4"), Some(Outcome::Failed { reason: "late".into() }));
    let hs = r.hs.lock().unwrap();
    assert!(hs.sends.is_empty() && hs.state_puts.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn homeserver_error_fails_the_call_and_reports_it() {
    let r = rig().await;
    r.hs.lock().unwrap().fail_sends = true;
    r.note.peer.send_call("c5", start(30, soon())).unwrap();
    let rec = r.note.rec.clone();
    eventually("failed", || rec.bodies("c5").contains(&CallBody::Ended)).await;
    assert!(matches!(outcome_of(&r, "c5"), Some(Outcome::Failed { .. })));
    assert!(cleared(&r), "the membership is cleared even when the ring failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn open_dm_is_idempotent_and_the_join_is_reported() {
    let r = rig().await;
    let first = r.note.peer.request(note_voice_proto::Request::OpenDm { link_id: 7, mxid: "@aki:t".into() }).await;
    let second = r.note.peer.request(note_voice_proto::Request::OpenDm { link_id: 7, mxid: "@aki:t".into() }).await;
    assert_eq!(first, second);
    let note_voice_proto::Reply::Dm { room_id } = first.unwrap() else { panic!() };
    {
        let hs = r.hs.lock().unwrap();
        assert_eq!(hs.created.len(), 1, "one room per link");
        assert_eq!(hs.created[0]["preset"], "trusted_private_chat");
        assert_eq!(hs.created[0]["is_direct"], true);
    }
    r.hs.lock().unwrap().syncs.push_back(joined_room(&room_id, vec![join_event("@aki:t")]));
    let rec = r.note.rec.clone();
    eventually("DmJoined reaches Note", || {
        rec.requests.lock().unwrap().iter().any(|q| matches!(q, note_voice_proto::Request::DmJoined { link_id: 7, .. }))
    })
    .await;
}
```

- [ ] **Step 4: Run to verify it fails**

Run: `cargo test -p note-voice`
Expected: compile errors (`note_voice` modules missing).

- [ ] **Step 5: Implement config and state**

`voice/src/config.rs`:

```rust
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct VoiceServiceConfig {
    pub homeserver: String,
    /// The bot's access token, alone in the file.
    pub token_file: PathBuf,
    pub livekit_service_url: String,
    pub socket: PathBuf,
    pub state_dir: PathBuf,
}

impl VoiceServiceConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        Ok(toml::from_str(&raw)?)
    }
}
```

`voice/src/state.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LinkState {
    pub mxid: String,
    pub room_id: String,
    pub reported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallState {
    pub room_id: String,
    pub done: bool,
    /// The seq of this side's `Ended` frame, once sent.
    pub ended_seq: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub links: BTreeMap<i64, LinkState>,
    pub calls: BTreeMap<String, CallState>,
    pub since: Option<String>,
}

/// `state.json`, replaced atomically on every save.
pub struct StateFile {
    path: PathBuf,
    pub data: State,
}

impl StateFile {
    pub fn open(dir: &Path) -> anyhow::Result<StateFile> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("state.json");
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(StateFile { path, data })
    }

    pub fn save(&self) -> std::io::Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(&self.data)?)?;
        f.sync_data()?;
        std::fs::rename(&tmp, &self.path)?;
        if let Some(dir) = self.path.parent() {
            std::fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    }
}
```

- [ ] **Step 6: Implement the Matrix client**

`voice/src/matrix.rs`:

```rust
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub const MEMBER_TYPE: &str = "org.matrix.msc3401.call.member";
pub const NOTIFICATION_TYPE: &str = "m.rtc.notification";
const DECLINE_TYPES: [&str; 2] = ["org.matrix.msc4310.rtc.decline", "m.rtc.decline"];
const USER_AGENT: &str = concat!("note-voice/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq)]
pub enum RoomEvent {
    Joined { room: String, user: String },
    CallMember { room: String, user: String, active: bool },
    Declined { room: String, notification: String },
}

#[derive(Debug, Clone)]
pub struct SyncBatch {
    pub next_batch: String,
    pub events: Vec<RoomEvent>,
}

pub struct Matrix {
    http: reqwest::Client,
    sync_http: reqwest::Client,
    base: String,
    token: String,
    pub user_id: String,
    pub device_id: String,
}

fn enc(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

impl Matrix {
    pub async fn connect(homeserver: &str, token: &str) -> Result<Matrix> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()?;
        let sync_http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(45))
            .build()?;
        let mut m = Matrix {
            http,
            sync_http,
            base: homeserver.trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
            user_id: String::new(),
            device_id: String::new(),
        };
        let who = m.get("/_matrix/client/v3/account/whoami").await.context("whoami")?;
        m.user_id = who["user_id"].as_str().context("whoami without user_id")?.to_string();
        m.device_id = who["device_id"].as_str().context("whoami without device_id")?.to_string();
        Ok(m)
    }

    async fn check(resp: reqwest::Response) -> Result<Value> {
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("homeserver said {status}: {}", body["errcode"].as_str().unwrap_or("?"));
        }
        Ok(body)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        Self::check(self.http.get(format!("{}{path}", self.base)).bearer_auth(&self.token).send().await?).await
    }

    async fn put(&self, path: &str, body: &Value) -> Result<Value> {
        Self::check(self.http.put(format!("{}{path}", self.base)).bearer_auth(&self.token).json(body).send().await?)
            .await
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        Self::check(self.http.post(format!("{}{path}", self.base)).bearer_auth(&self.token).json(body).send().await?)
            .await
    }

    pub fn member_key(&self) -> String {
        format!("_{}_{}_m.call", self.user_id, self.device_id)
    }

    /// An unencrypted DM inviting `mxid`, recorded in the bot's `m.direct`.
    pub async fn create_dm(&self, mxid: &str) -> Result<String> {
        let created = self
            .post(
                "/_matrix/client/v3/createRoom",
                &json!({ "preset": "trusted_private_chat", "is_direct": true, "invite": [mxid], "name": "Note" }),
            )
            .await?;
        let room = created["room_id"].as_str().context("createRoom without room_id")?.to_string();
        let path = format!("/_matrix/client/v3/user/{}/account_data/m.direct", enc(&self.user_id));
        let mut direct = self.get(&path).await.unwrap_or_else(|_| json!({}));
        if !direct.is_object() {
            direct = json!({});
        }
        let rooms = direct[mxid].as_array().cloned().unwrap_or_default();
        let mut rooms: Vec<Value> = rooms.into_iter().filter(|r| r != &json!(room)).collect();
        rooms.push(json!(room));
        direct[mxid] = json!(rooms);
        self.put(&path, &direct).await?;
        Ok(room)
    }

    pub async fn invite(&self, room: &str, mxid: &str) -> Result<()> {
        self.post(&format!("/_matrix/client/v3/rooms/{}/invite", enc(room)), &json!({ "user_id": mxid }))
            .await
            .map(|_| ())
    }

    pub async fn put_member(&self, room: &str, expires_ms: u64, livekit_url: &str) -> Result<String> {
        let path = format!(
            "/_matrix/client/v3/rooms/{}/state/{MEMBER_TYPE}/{}",
            enc(room),
            enc(&self.member_key())
        );
        let body = json!({
            "application": "m.call",
            "call_id": "",
            "scope": "m.room",
            "device_id": self.device_id,
            "expires": expires_ms,
            "focus_active": { "type": "livekit", "focus_selection": "oldest_membership" },
            "foci_preferred": [{ "type": "livekit", "livekit_service_url": livekit_url, "livekit_alias": room }],
            "m.call.intent": "audio",
        });
        let got = self.put(&path, &body).await?;
        Ok(got["event_id"].as_str().context("state put without event_id")?.to_string())
    }

    pub async fn clear_member(&self, room: &str) -> Result<()> {
        let path = format!(
            "/_matrix/client/v3/rooms/{}/state/{MEMBER_TYPE}/{}",
            enc(room),
            enc(&self.member_key())
        );
        self.put(&path, &json!({})).await.map(|_| ())
    }

    pub async fn ring(&self, room: &str, target: &str, member_event_id: &str, lifetime_ms: u64) -> Result<String> {
        let sender_ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as u64;
        let path = format!(
            "/_matrix/client/v3/rooms/{}/send/{NOTIFICATION_TYPE}/{}",
            enc(room),
            uuid::Uuid::new_v4().simple()
        );
        let body = json!({
            "sender_ts": sender_ts,
            "lifetime": lifetime_ms,
            "notification_type": "ring",
            "m.call.intent": "audio",
            "m.mentions": { "user_ids": [target], "room": true },
            "m.relates_to": { "rel_type": "m.reference", "event_id": member_event_id },
        });
        let got = self.put(&path, &body).await?;
        Ok(got["event_id"].as_str().context("send without event_id")?.to_string())
    }

    pub async fn sync(&self, since: Option<&str>, timeout_ms: u64) -> Result<SyncBatch> {
        let filter = json!({
            "presence": { "not_types": ["*"] },
            "account_data": { "not_types": ["*"] },
            "room": {
                "account_data": { "not_types": ["*"] },
                "ephemeral": { "not_types": ["*"] },
                "timeline": { "limit": 50 },
            },
        })
        .to_string();
        let mut query: Vec<(&str, String)> = vec![("timeout", timeout_ms.to_string()), ("filter", filter)];
        if let Some(s) = since {
            query.push(("since", s.to_string()));
        }
        let resp = self
            .sync_http
            .get(format!("{}/_matrix/client/v3/sync", self.base))
            .bearer_auth(&self.token)
            .query(&query)
            .send()
            .await?;
        let body = Self::check(resp).await?;
        Ok(SyncBatch {
            next_batch: body["next_batch"].as_str().context("sync without next_batch")?.to_string(),
            events: parse_sync(&body),
        })
    }
}

fn parse_sync(body: &Value) -> Vec<RoomEvent> {
    let mut out = Vec::new();
    let Some(rooms) = body["rooms"]["join"].as_object() else { return out };
    for (room, data) in rooms {
        let lists = [&data["state"]["events"], &data["timeline"]["events"]];
        for ev in lists.into_iter().filter_map(|l| l.as_array()).flatten() {
            let kind = ev["type"].as_str().unwrap_or_default();
            let sender = ev["sender"].as_str().unwrap_or_default().to_string();
            if kind == "m.room.member" && ev["content"]["membership"] == "join" {
                if let Some(user) = ev["state_key"].as_str() {
                    out.push(RoomEvent::Joined { room: room.clone(), user: user.to_string() });
                }
            } else if kind == MEMBER_TYPE {
                let active = ev["content"].as_object().is_some_and(|c| !c.is_empty());
                out.push(RoomEvent::CallMember { room: room.clone(), user: sender, active });
            } else if DECLINE_TYPES.contains(&kind) {
                if let Some(id) = ev["content"]["m.relates_to"]["event_id"].as_str() {
                    out.push(RoomEvent::Declined { room: room.clone(), notification: id.to_string() });
                }
            }
        }
    }
    out
}
```

- [ ] **Step 7: Implement one outbound ring**

`voice/src/calls.rs`:

```rust
use crate::matrix::{Matrix, RoomEvent};
use note_voice_proto::Outcome;
use std::time::Duration;
use tokio::sync::{broadcast, watch};

/// How long the bot's call membership outlives the ring, so a slow answer
/// still finds a live call.
const MEMBER_GRACE_SECS: u64 = 30;

pub struct Ring<'a> {
    pub matrix: &'a Matrix,
    pub room_id: &'a str,
    pub mxid: &'a str,
    pub livekit_url: &'a str,
    pub ring_secs: u32,
}

/// Rings once and returns how it ended; the membership is cleared on every
/// path. `on_ringing` fires once the ring event is out.
pub async fn ring_once(
    r: Ring<'_>,
    mut events: broadcast::Receiver<RoomEvent>,
    mut hang_up: watch::Receiver<bool>,
    on_ringing: impl FnOnce(),
) -> Outcome {
    let outcome = ring_inner(&r, &mut events, &mut hang_up, on_ringing).await;
    if let Err(e) = r.matrix.clear_member(r.room_id).await {
        eprintln!("voice: clearing the call membership in {} failed: {e:#}", r.room_id);
    }
    outcome
}

async fn ring_inner(
    r: &Ring<'_>,
    events: &mut broadcast::Receiver<RoomEvent>,
    hang_up: &mut watch::Receiver<bool>,
    on_ringing: impl FnOnce(),
) -> Outcome {
    let failed = |e: anyhow::Error| Outcome::Failed { reason: format!("{e:#}") };
    let expires = (r.ring_secs as u64 + MEMBER_GRACE_SECS) * 1000;
    let member = match r.matrix.put_member(r.room_id, expires, r.livekit_url).await {
        Ok(id) => id,
        Err(e) => return failed(e),
    };
    let notification = match r.matrix.ring(r.room_id, r.mxid, &member, r.ring_secs as u64 * 1000).await {
        Ok(id) => id,
        Err(e) => return failed(e),
    };
    on_ringing();
    let deadline = tokio::time::sleep(Duration::from_secs(r.ring_secs as u64));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return Outcome::Missed,
            changed = hang_up.changed() => {
                if changed.is_err() || *hang_up.borrow() {
                    return Outcome::Failed { reason: "hung up by Note".into() };
                }
            }
            ev = events.recv() => match ev {
                Ok(RoomEvent::CallMember { room, user, active: true }) if room == r.room_id && user == r.mxid => {
                    return Outcome::Answered;
                }
                Ok(RoomEvent::Declined { room, notification: n }) if room == r.room_id && n == notification => {
                    return Outcome::Declined;
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    return Outcome::Failed { reason: "the sync loop stopped".into() };
                }
            }
        }
    }
}
```

- [ ] **Step 8: Implement the service**

`voice/src/service.rs`:

```rust
use crate::calls::{ring_once, Ring};
use crate::config::VoiceServiceConfig;
use crate::matrix::{Matrix, RoomEvent};
use crate::state::{CallState, LinkState, StateFile};
use note_voice_proto::{
    dial_forever, AppliedFile, BoxFuture, CallBody, Dir, FileOutbox, Handler, Outcome, Peer, PeerConfig,
    Refusal, RefusalCode, Reply, Request, Role,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{broadcast, watch};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

struct Service {
    cfg: VoiceServiceConfig,
    matrix: Arc<Matrix>,
    state: Mutex<StateFile>,
    applied: AppliedFile,
    events: broadcast::Sender<RoomEvent>,
    hang_ups: Mutex<HashMap<String, watch::Sender<bool>>>,
    peer: OnceLock<Peer>,
}

impl Service {
    fn peer(&self) -> &Peer {
        self.peer.get().expect("the peer is set before anything runs")
    }

    fn send(&self, call_id: &str, body: CallBody) -> Option<u64> {
        match self.peer().send_call(call_id, body) {
            Ok(seq) => Some(seq),
            Err(e) => {
                eprintln!("voice: journaling a frame for {call_id} failed: {e}");
                None
            }
        }
    }

    fn finish(&self, call_id: &str, outcome: Outcome) {
        self.send(call_id, CallBody::Outcome { outcome });
        let ended = self.send(call_id, CallBody::Ended);
        let mut st = lock(&self.state);
        if let Some(c) = st.data.calls.get_mut(call_id) {
            c.done = true;
            c.ended_seq = ended;
        }
        let _ = st.save();
        lock(&self.hang_ups).remove(call_id);
    }

    fn begin_ring(self: &Arc<Self>, call_id: String, room_id: String, mxid: String, ring_secs: u32) {
        let (tx, rx) = watch::channel(false);
        lock(&self.hang_ups).insert(call_id.clone(), tx);
        let events = self.events.subscribe();
        let svc = self.clone();
        tokio::spawn(async move {
            let ring = Ring {
                matrix: &svc.matrix,
                room_id: &room_id,
                mxid: &mxid,
                livekit_url: &svc.cfg.livekit_service_url,
                ring_secs,
            };
            let id = call_id.clone();
            let s = svc.clone();
            let outcome = ring_once(ring, events, rx, move || {
                s.send(&id, CallBody::Ringing);
            })
            .await;
            svc.finish(&call_id, outcome);
        });
    }

    /// Calls a crash left open cannot be resumed; each is closed and reported.
    async fn recover(self: &Arc<Self>) {
        let open: Vec<(String, String)> = lock(&self.state)
            .data
            .calls
            .iter()
            .filter(|(_, c)| !c.done)
            .map(|(id, c)| (id.clone(), c.room_id.clone()))
            .collect();
        for (call_id, room_id) in open {
            let _ = self.matrix.clear_member(&room_id).await;
            self.finish(&call_id, Outcome::Failed { reason: "the voice service restarted".into() });
        }
    }

    async fn open_dm(&self, link_id: i64, mxid: String) -> Result<Reply, Refusal> {
        let known = lock(&self.state).data.links.get(&link_id).cloned();
        if let Some(link) = known.filter(|l| l.mxid == mxid) {
            let _ = self.matrix.invite(&link.room_id, &mxid).await;
            return Ok(Reply::Dm { room_id: link.room_id });
        }
        let room_id = self
            .matrix
            .create_dm(&mxid)
            .await
            .map_err(|e| Refusal::new(RefusalCode::Failed, format!("{e:#}")))?;
        let mut st = lock(&self.state);
        st.data.links.insert(link_id, LinkState { mxid, room_id: room_id.clone(), reported: false });
        st.save().map_err(|e| Refusal::new(RefusalCode::Failed, e.to_string()))?;
        Ok(Reply::Dm { room_id })
    }

    async fn report_join(self: &Arc<Self>, room: &str, user: &str) {
        let pending: Vec<i64> = lock(&self.state)
            .data
            .links
            .iter()
            .filter(|(_, l)| l.room_id == room && l.mxid == user && !l.reported)
            .map(|(id, _)| *id)
            .collect();
        for link_id in pending {
            let svc = self.clone();
            let room = room.to_string();
            tokio::spawn(async move {
                loop {
                    let got = svc.peer().request(Request::DmJoined { link_id, room_id: room.clone() }).await;
                    if got.is_ok() {
                        let mut st = lock(&svc.state);
                        if let Some(l) = st.data.links.get_mut(&link_id) {
                            l.reported = true;
                        }
                        let _ = st.save();
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            });
        }
    }

    async fn sync_forever(self: Arc<Self>) {
        loop {
            let since = lock(&self.state).data.since.clone();
            match self.matrix.sync(since.as_deref(), 30_000).await {
                Ok(batch) => {
                    for ev in batch.events {
                        if let RoomEvent::Joined { room, user } = &ev {
                            self.report_join(room, user).await;
                        }
                        let _ = self.events.send(ev);
                    }
                    let mut st = lock(&self.state);
                    st.data.since = Some(batch.next_batch);
                    let _ = st.save();
                }
                Err(e) => {
                    eprintln!("voice: sync failed: {e:#}");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    }
}

struct VoiceHandler {
    svc: OnceLock<Arc<Service>>,
}

impl VoiceHandler {
    fn svc(&self) -> &Arc<Service> {
        self.svc.get().expect("the service is set before the link runs")
    }
}

impl Handler for VoiceHandler {
    fn applied(&self, call_id: &str) -> u64 {
        self.svc().applied.applied(call_id)
    }

    fn apply(&self, call_id: &str, seq: u64, body: CallBody) -> Result<(), String> {
        let svc = self.svc().clone();
        match body {
            CallBody::Start { room_id, mxid, ring_secs, ring_by_ms, .. } => {
                {
                    let mut st = lock(&svc.state);
                    st.data.calls.insert(
                        call_id.to_string(),
                        CallState { room_id: room_id.clone(), done: false, ended_seq: None },
                    );
                    st.save().map_err(|e| e.to_string())?;
                }
                svc.applied.set_applied(call_id, seq).map_err(|e| e.to_string())?;
                if now_ms() > ring_by_ms {
                    svc.finish(call_id, Outcome::Failed { reason: "late".into() });
                } else {
                    svc.begin_ring(call_id.to_string(), room_id, mxid, ring_secs);
                }
            }
            CallBody::HangUp => {
                if let Some(tx) = lock(&svc.hang_ups).get(call_id) {
                    let _ = tx.send(true);
                }
                svc.applied.set_applied(call_id, seq).map_err(|e| e.to_string())?;
            }
            _ => svc.applied.set_applied(call_id, seq).map_err(|e| e.to_string())?,
        }
        Ok(())
    }

    fn request(&self, body: Request) -> BoxFuture<Result<Reply, Refusal>> {
        let svc = self.svc().clone();
        Box::pin(async move {
            match body {
                Request::OpenDm { link_id, mxid } => svc.open_dm(link_id, mxid).await,
                Request::DmJoined { .. } => Err(Refusal::new(RefusalCode::BadRequest, "the voice side reports joins")),
            }
        })
    }

    /// Once Note holds the `Ended` frame, nothing of the call is kept here.
    fn acked(&self, call_id: &str, upto: u64) {
        let svc = self.svc();
        let done = lock(&svc.state).data.calls.get(call_id).and_then(|c| c.ended_seq).is_some_and(|s| upto >= s);
        if done {
            let _ = svc.peer().forget(call_id);
            let _ = svc.applied.forget(call_id);
            let mut st = lock(&svc.state);
            st.data.calls.remove(call_id);
            let _ = st.save();
        }
    }
}

pub async fn run(cfg: VoiceServiceConfig) -> anyhow::Result<()> {
    run_with(cfg, PeerConfig::new(Role::Voice)).await
}

pub async fn run_with(cfg: VoiceServiceConfig, peer_cfg: PeerConfig) -> anyhow::Result<()> {
    let token = std::fs::read_to_string(&cfg.token_file)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", cfg.token_file.display()))?;
    let matrix = Arc::new(Matrix::connect(&cfg.homeserver, &token).await?);
    eprintln!("voice: signed in as {} ({})", matrix.user_id, matrix.device_id);
    let journal = FileOutbox::open(&cfg.state_dir.join("journal"))?;
    let applied = AppliedFile::new(&cfg.state_dir.join("journal"));
    let state = StateFile::open(&cfg.state_dir)?;
    let svc = Arc::new(Service {
        cfg: cfg.clone(),
        matrix,
        state: Mutex::new(state),
        applied,
        events: broadcast::channel(256).0,
        hang_ups: Mutex::new(HashMap::new()),
        peer: OnceLock::new(),
    });
    let handler = Arc::new(VoiceHandler { svc: OnceLock::new() });
    let _ = handler.svc.set(svc.clone());
    let peer = Peer::new(peer_cfg, Dir::ToNote, handler, Box::new(journal));
    let _ = svc.peer.set(peer.clone());
    svc.recover().await;
    tokio::spawn(svc.clone().sync_forever());
    dial_forever(peer, cfg.socket.clone()).await;
    Ok(())
}
```

`voice/src/lib.rs`:

```rust
pub mod calls;
pub mod config;
pub mod matrix;
pub mod service;
pub mod state;
```

`voice/src/main.rs`:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::var_os("NOTE_VOICE_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("note-voice.toml"));
    let cfg = note_voice::config::VoiceServiceConfig::load(&path)?;
    note_voice::service::run(cfg).await
}
```

- [ ] **Step 9: Run the suite**

Run: `cargo test -p note-voice`
Expected: all 6 pass. Run five times; every run passes.

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml Cargo.lock voice
git commit -m "feat(voice): note-voice rings a linked account through Matrix and reports how the ring ended"
```

---

### Task 10: Restart safety across both processes

**Files:**
- Test: `voice/tests/ring.rs` (append)

**Interfaces:**
- Consumes: `service::run_with`, `FakeNote`.

- [ ] **Step 1: Write the failing test**

Append to `voice/tests/ring.rs`:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn a_voice_restart_mid_ring_closes_the_call_and_reports_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let (base, hs) = homeserver().await;
    let token = dir.path().join("token");
    std::fs::write(&token, "secret").unwrap();
    let socket = dir.path().join("voice.sock");
    let note = fake_note(&socket);
    let cfg = VoiceServiceConfig {
        homeserver: base,
        token_file: token,
        livekit_service_url: "https://rtc.t".into(),
        socket,
        state_dir: dir.path().join("state"),
    };
    let first = {
        let cfg = cfg.clone();
        tokio::spawn(async move { note_voice::service::run_with(cfg, fast(Role::Voice)).await.unwrap() })
    };
    let p = note.peer.clone();
    eventually("voice connects", || p.is_up()).await;
    note.peer.send_call("c9", start(30, soon())).unwrap();
    let rec = note.rec.clone();
    eventually("ringing", || rec.bodies("c9").contains(&CallBody::Ringing)).await;

    first.abort();
    let p = note.peer.clone();
    eventually("Note sees the voice side gone", || !p.is_up()).await;
    tokio::spawn(async move { note_voice::service::run_with(cfg, fast(Role::Voice)).await.unwrap() });

    let rec = note.rec.clone();
    eventually("the call is closed after the restart", || rec.bodies("c9").contains(&CallBody::Ended)).await;
    let outcomes: Vec<Outcome> = note
        .rec
        .bodies("c9")
        .into_iter()
        .filter_map(|b| match b {
            CallBody::Outcome { outcome } => Some(outcome),
            _ => None,
        })
        .collect();
    assert_eq!(outcomes, vec![Outcome::Failed { reason: "the voice service restarted".into() }]);
    assert_eq!(note.rec.seqs("c9"), (1..=note.rec.seqs("c9").len() as u64).collect::<Vec<_>>());
    let cleared = hs.lock().unwrap().state_puts.iter().filter(|(_, _, _, b)| b == &serde_json::json!({})).count();
    assert!(cleared >= 1, "the orphaned membership is cleared");
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p note-voice --test ring a_voice_restart`
Expected: PASS if Task 9 is right. If it fails, fix the service, not the test: recovery must close the call, and the journal must replay the `Ringing` frame with its original seq.

- [ ] **Step 3: Commit**

```bash
git add voice
git commit -m "test(voice): a restart mid-ring closes the call and reports it once"
```

---

### Task 11: Nix packages and the NixOS module

**Files:**
- Modify: `flake.nix`
- Modify: `nix/module.nix`
- Modify: `nix/test.nix` (the existing VM test must still pass)

**Interfaces:**
- Produces:
  - `packages.<system>.note-voice`.
  - `services.note.voice.{enable, package, settings, credentials}`.
  - `note.service` gets `RuntimeDirectory=note` (0750) and writes `[voice] socket = "/run/note/voice.sock"` when voice is enabled.
  - A `note-voice.service` unit.

- [ ] **Step 1: Flake package**

In `flake.nix`:
- Widen both filesets to `[ ./Cargo.toml ./Cargo.lock ./server ./voice-proto ./voice ]`.
- Add `cargoExtraArgs = "-p note-server";` to the server's `buildPackage` args.
- Add:

```nix
          voice = craneLib.buildPackage (commonArgs // {
            pname = "note-voice";
            inherit cargoArtifacts;
            cargoExtraArgs = "-p note-voice";
            doCheck = false;
            meta = {
              description = "Rings a linked Matrix account for Note";
              license = lib.licenses.unlicense;
              mainProgram = "note-voice";
            };
          });
```

- Return `{ inherit web server voice tests; }`.
- Expose `note-voice = b.voice;` in `packages`.
- In `overlays.default`, add `note-voice = (build final).voice;`.

- [ ] **Step 2: Build**

Run: `nix build .#note-server .#note-voice`
Expected: both build. `pnpmDeps.hash` is unaffected (no `web/pnpm-lock.yaml` change).

- [ ] **Step 3: Module options and units**

In `nix/module.nix`, inside `options.services.note`:

```nix
    voice = {
      enable = lib.mkEnableOption "the voice service that rings a linked Matrix account";
      package = lib.mkOption {
        type = lib.types.package;
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.note-voice;
        defaultText = lib.literalExpression "note.packages.\${system}.note-voice";
      };
      settings = lib.mkOption {
        type = lib.types.submodule {
          freeformType = toml.type;
          options = {
            homeserver = lib.mkOption { type = lib.types.str; example = "https://matrix.example.com"; };
            livekit_service_url = lib.mkOption { type = lib.types.str; };
          };
        };
        default = { };
        description = "note-voice.toml; socket, state_dir and token_file are filled in.";
      };
      tokenFile = lib.mkOption {
        type = lib.types.path;
        description = "The bot account's access token, handed over with LoadCredential.";
      };
    };
```

In `let`:

```nix
  voiceCfg = cfg.voice;
  voiceSocket = "/run/note/voice.sock";
  voiceToml = toml.generate "note-voice.toml" (voiceCfg.settings // {
    socket = voiceSocket;
    state_dir = "/var/lib/note-voice";
    token_file = "/run/credentials/note-voice.service/matrix-bot.token";
  });
```

Use `lib.recursiveUpdate cfg.settings (lib.optionalAttrs voiceCfg.enable { voice.socket = voiceSocket; })` in place of `cfg.settings` when generating `serverToml`.

In `systemd.services.note.serviceConfig`, add:

```nix
        RuntimeDirectory = "note";
        RuntimeDirectoryMode = "0750";
```

In `config`:

```nix
    users.users.note-voice = lib.mkIf voiceCfg.enable {
      isSystemUser = true;
      group = cfg.group;
    };

    systemd.services.note-voice = lib.mkIf voiceCfg.enable {
      description = "Note voice service";
      wantedBy = [ "multi-user.target" ];
      after = [ "note.service" "network-online.target" ];
      wants = [ "network-online.target" ];
      environment.NOTE_VOICE_CONFIG = "${voiceToml}";
      unitConfig.StartLimitIntervalSec = 0;
      serviceConfig = {
        ExecStart = lib.getExe voiceCfg.package;
        User = "note-voice";
        Group = cfg.group;
        StateDirectory = "note-voice";
        StateDirectoryMode = "0700";
        LoadCredential = [ "matrix-bot.token:${voiceCfg.tokenFile}" ];
        Restart = "always";
        RestartSec = 2;
        RestartSteps = 5;
        RestartMaxDelaySec = 60;
        UMask = "0077";
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
        ProtectProc = "invisible";
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [ "@system-service" "~@privileged" ];
        CapabilityBoundingSet = "";
      };
    };
```

Phase 2 relaxes `PrivateDevices` and `MemoryDenyWriteExecute` and adds `DeviceAllow` for the GPU, with the audio stack. Phase 1 needs neither.

Change `note.service`'s `Restart` from `"on-failure"` to `"always"` (the spec's supervision).

- [ ] **Step 4: Checks**

Run: `nix flake check`
Expected: `note-server-tests` and the `module` VM test pass. If `nix/test.nix` builds the server config from `cfg.settings`, it is unaffected, since voice stays disabled there.

- [ ] **Step 5: Commit**

```bash
git add flake.nix nix
git commit -m "feat(nix): package note-voice and run it beside Note with the socket in /run/note"
```

---

### Task 12: Deploy and ring the phone

This task changes the host and needs the user for the `sudo` steps. Each step says who runs it.

- [ ] **Step 1 (user): Install the bot token.** Claude prints the token from the PoC state file so the user can pipe it:

```
! python3 -c "import json;print(json.load(open('/tmp/claude-1000/-home-shuntia-Projects-note/f12bc893-a0bd-45ea-b8e5-5e5312ea6c24/scratchpad/matrix-ring/state.json'))['access_token'])" | sudo install -m600 -o root -g root /dev/stdin /persist/secrets/note/matrix-bot.token
```

If the scratchpad is gone, log the bot in again with its password from the same file, or register a fresh bot as in the spec's groundwork. The password and token belong only in `/persist/secrets/note/`.

- [ ] **Step 2 (Claude): Host module.** In `~/Documents/configuration-nix/modules/note.nix`, inside `services.note`:

```nix
    voice = {
      enable = true;
      tokenFile = "${secrets}/matrix-bot.token";
      settings = {
        homeserver = "https://uwu.shuntia.net";
        livekit_service_url = "https://matrix-rtc-uwu.shuntia.net";
      };
    };
```

The voice service's state (`state.json`, the journal of frames Note has not acknowledged, and the sync token) must survive a reboot, so in the same file add to `environment.persistence."/persist".directories`:

```nix
    { directory = "/var/lib/note-voice"; user = "note-voice"; group = "note"; mode = "0700"; }
```

Also add `#   matrix-bot.token  access token of @note:uwu.shuntia.net (the voice service's bot)` to the secrets list in the header comment.

- [ ] **Step 3 (Claude): Bump and build.** Merge the branch to `main`, then in `~/Documents/configuration-nix` run `nix flake update note` and `nix build .#nixosConfigurations.$(hostname).config.system.build.toplevel --no-link`. Confirm that `note-voice.service` exists in the build and that the generated `server.toml` holds `[voice] socket = "/run/note/voice.sock"`.

- [ ] **Step 4 (user): Switch.** `sudo nixos-rebuild switch`. v42 migrates at start.

- [ ] **Step 5 (Claude): Verify the link.** Run `journalctl -u note-voice -n 50` (readable by the user's group, or ask the user). Expect `voice: signed in as @note:uwu.shuntia.net` and no repeated `voice link dropped`.

- [ ] **Step 6 (user, then Claude): Link and ring.**
  - In Settings → Calls, the user links `@shuntia:uwu.shuntia.net` and accepts "Note" in Element X. This is a new room, separate from the PoC DM.
  - Then the user taps **Ring me**. `POST /api/voice/test` runs in the user's own session, so the tap is theirs.
  - The `call-shuntia-for-testing` memory lets Claude ask for further test rings as needed.

Expected:
- The iPhone shows the CallKit incoming call.
- Answering ends it at once.
- "This was a test call from Note." arrives via Telegram or push.
- `voice_calls` shows `ended/answered` (via `sudo note-ctl`, or the admin view when one is added).

Then a decline check: the user taps **Ring me** again and declines on the phone. Expected: the ring stops at once, the test message still arrives, and `voice_calls` shows `ended/declined`.

Finally, `journalctl -u note-voice -b` shows no sandbox denials: no `Permission denied`, `Read-only file system` or `Operation not permitted` on `/var/lib/note-voice` or `/run/note/voice.sock`, and no `SIGSYS` exit.

- [ ] **Step 7 (Claude): Update memory.** Record in `note-deployment.md` the deployed commit, DB v42, `note-voice.service`, and the token location. Mark phase 1 done in `matrix-ring-poc.md`.

---

## Self-Review Notes

**Spec coverage (phase 1 slice):**

| Spec requirement | Task |
|---|---|
| Transport | 3, 7, 11 |
| Frames and requests | 1, 3 |
| Exactly-once (seq, ack, replay, dedupe) | 2, 3, 4, 5 |
| Supervision (`Restart=always`, recovery) | 9, 10, 11 |
| Degraded "link down at ring time" | 7 |
| Fault-injection tests | 3, 7, 10 |
| Linking | 5, 6, 8, 9 |
| Outbound ring with fallthrough on every outcome | 6, 7, 9 |
| Data tables (`voice_ops` created now, used in phase 3) | 5 |
| Configuration | 6, 7, 11 |

Deferred to later phases:
- **Watchdog (`sd_notify`).** Deferred to phase 2, where the voice process gains real-time work worth watching. `Restart=always` plus heartbeats cover phase 1.
- **The rest.** The brief, the audio pipeline, the cues, voice selection, `respond`, drafts, inbound calls and the language seams are phases 2–4.

**Type consistency:** `Voice::start_call(user_id, &Link, &OutboundMessage, Timestamp)` is used identically in Tasks 6, 7 and 8. `Peer::new(cfg, out_dir, handler, outbox)` is the same in Tasks 3, 6, 7 and 9. `CallBody` variants match across Tasks 1, 6 and 9.
