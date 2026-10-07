use serde::{Deserialize, Serialize};

/// Bumped on any change to a frame's shape; both sides must agree exactly.
pub const PROTO_VERSION: u32 = 3;

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

/// Who placed the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    #[default]
    Outbound,
    Inbound,
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
    /// Voice → Note: the linked user is calling in `room_id`; `key` names this call attempt (the call.member event id).
    IncomingCall { room_id: String, mxid: String, key: String },
    /// Note → voice: the voices on offer for `language`.
    ListVoices { language: String },
    /// Note → voice: a short sample of `voice`, as 24 kHz mono 16-bit WAV, base64.
    Preview { language: String, voice: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Reply {
    Dm { room_id: String },
    Done,
    /// The call Note opened for an `IncomingCall`; its `Start` follows on the call stream.
    Call { call_id: String },
    Voices { voices: Vec<VoiceOption> },
    Audio { wav_base64: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceOption {
    pub id: String,
    pub label: String,
    pub language: String,
    /// The label of the speech model the voice runs on, which voices are grouped by.
    #[serde(default)]
    pub backend: String,
    /// Slow to start speaking.
    #[serde(default)]
    pub slow: bool,
    /// The attribution its licence asks to be shown with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit: Option<String>,
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
        #[serde(default)]
        voice: VoiceProfile,
        #[serde(default)]
        direction: Direction,
    },
    /// Note → voice: end the call now, ringing or not.
    HangUp,
    /// Voice → Note: the phone is ringing.
    Ringing,
    /// Voice → Note: how the ring ended.
    Outcome { outcome: Outcome },
    /// Voice → Note: the voice side holds nothing more for this call.
    Ended,
    /// Note → voice: one clause of reply `reply`, in order by `idx`.
    Speak { reply: u64, idx: u32, text: String },
    /// Note → voice: reply `reply` has no more clauses.
    SpeakDone { reply: u64 },
    /// Note → voice: reply `reply` may play once its turn is committed (wake replies are played at once).
    Play { reply: u64 },
    /// Note → voice: discard reply `reply` unplayed.
    Drop { reply: u64 },
    /// Voice → Note: the user paused; `text` is the transcript so far of turn `turn`.
    Draft {
        turn: u64,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
    },
    /// Voice → Note: the turn is over. `language` is the one the call settled on from the caller's
    /// first words; a voice side that does not identify it leaves it out.
    Commit {
        turn: u64,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
    },
    /// Voice → Note: the user resumed speaking after a draft; the draft is void.
    Retract { turn: u64 },
    /// Voice → Note: floor changes.
    Floor { floor: Floor },
    /// Voice → Note: the user cut reply `reply` off; `heard_chars` of its text were played.
    BargeIn { reply: u64, heard_chars: u32 },
    /// Voice → Note: reply `reply` played through to its end.
    Played { reply: u64 },
}

/// `UserQuiet`: VAD sees silence after speech. `Drained`: the playout queue emptied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Floor {
    UserSpeaking,
    UserQuiet,
    Drained,
}

/// `voice` empty means the language's default voice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceProfile {
    pub language: String,
    pub voice: String,
    pub cue: bool,
}

impl Default for VoiceProfile {
    fn default() -> Self {
        Self { language: "en".into(), voice: String::new(), cue: true }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_v1_start_without_a_voice_profile_reads_with_defaults() {
        let raw = r#"{"k":"start","user_id":1,"room_id":"!r","mxid":"@a","title":"t","ring_secs":30,"ring_by_ms":5}"#;
        let body: CallBody = serde_json::from_str(raw).unwrap();
        let CallBody::Start { voice, .. } = body else { panic!() };
        assert_eq!(voice, VoiceProfile { language: "en".into(), voice: String::new(), cue: true });
    }

    #[test]
    fn every_conversation_frame_round_trips() {
        for body in [
            CallBody::Speak { reply: 3, idx: 0, text: "Hey,".into() },
            CallBody::SpeakDone { reply: 3 },
            CallBody::Play { reply: 3 },
            CallBody::Drop { reply: 3 },
            CallBody::Draft { turn: 2, text: "move my".into(), language: None },
            CallBody::Commit { turn: 2, text: "move my run".into(), language: Some("ja".into()) },
            CallBody::Retract { turn: 2 },
            CallBody::Floor { floor: Floor::UserSpeaking },
            CallBody::BargeIn { reply: 3, heard_chars: 12 },
            CallBody::Played { reply: 3 },
        ] {
            let s = serde_json::to_string(&body).unwrap();
            assert_eq!(serde_json::from_str::<CallBody>(&s).unwrap(), body, "{s}");
        }
    }

    #[test]
    fn a_turn_without_a_language_reads_and_writes_as_before() {
        let raw = r#"{"k":"commit","turn":1,"text":"hi"}"#;
        let body: CallBody = serde_json::from_str(raw).unwrap();
        assert_eq!(body, CallBody::Commit { turn: 1, text: "hi".into(), language: None });
        assert_eq!(serde_json::to_string(&body).unwrap(), raw);
        let raw = r#"{"k":"draft","turn":1,"text":"hi","language":"ja"}"#;
        let CallBody::Draft { language, .. } = serde_json::from_str(raw).unwrap() else { panic!() };
        assert_eq!(language.as_deref(), Some("ja"));
    }

    #[test]
    fn a_start_without_a_direction_reads_as_outbound() {
        let raw = r#"{"k":"start","user_id":1,"room_id":"!r","mxid":"@a","title":"t","ring_secs":30,"ring_by_ms":5}"#;
        let CallBody::Start { direction, .. } = serde_json::from_str(raw).unwrap() else { panic!() };
        assert_eq!(direction, Direction::Outbound);
    }

    #[test]
    fn an_inbound_start_round_trips() {
        let body = CallBody::Start {
            user_id: 1,
            room_id: "!r".into(),
            mxid: "@a".into(),
            title: "t".into(),
            ring_secs: 30,
            ring_by_ms: 5,
            voice: VoiceProfile::default(),
            direction: Direction::Inbound,
        };
        let s = serde_json::to_string(&body).unwrap();
        assert!(s.contains(r#""direction":"inbound""#), "{s}");
        assert_eq!(serde_json::from_str::<CallBody>(&s).unwrap(), body);
    }

    #[test]
    fn every_inbound_and_preview_request_and_reply_round_trips() {
        for req in [
            Request::IncomingCall { room_id: "!r".into(), mxid: "@a".into(), key: "$ev".into() },
            Request::ListVoices { language: "ja".into() },
            Request::Preview { language: "en".into(), voice: "af_heart".into() },
        ] {
            let s = serde_json::to_string(&req).unwrap();
            assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), req, "{s}");
        }
        for reply in [
            Reply::Call { call_id: "c-1".into() },
            Reply::Voices {
                voices: vec![
                    VoiceOption {
                        id: "af_heart".into(),
                        label: "Heart".into(),
                        language: "en".into(),
                        backend: "Kokoro".into(),
                        slow: false,
                        credit: None,
                    },
                    VoiceOption {
                        id: "kyutai:alba".into(),
                        label: "Alba".into(),
                        language: "en".into(),
                        backend: "Natural".into(),
                        slow: true,
                        credit: Some("VOICEVOX:冥鳴ひまり".into()),
                    },
                ],
            },
            Reply::Audio { wav_base64: "UklGRg==".into() },
        ] {
            let s = serde_json::to_string(&reply).unwrap();
            assert_eq!(serde_json::from_str::<Reply>(&s).unwrap(), reply, "{s}");
        }
    }
}
