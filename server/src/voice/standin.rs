use crate::channels::OutboundMessage;
use note_voice_proto::{CallBody, Outcome};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

/// What Note says on a live call. `on_frame` sees each frame of a live call
/// after it is stored, and is never called while the DB guard is held.
pub trait Conversation: Send + Sync {
    fn on_frame(&self, call_id: &str, body: &CallBody, out: &dyn Fn(CallBody));
}

/// Speaks the call's message on answer, then echoes each committed turn.
pub struct StandIn {
    db: Arc<Mutex<Connection>>,
}

impl StandIn {
    pub fn new(db: Arc<Mutex<Connection>>) -> StandIn {
        StandIn { db }
    }

    fn message(&self, call_id: &str) -> Option<OutboundMessage> {
        let raw: String = crate::db_guard(&self.db)
            .query_row("SELECT message FROM voice_calls WHERE id = ?1", [call_id], |r| r.get(0))
            .ok()?;
        serde_json::from_str(&raw).ok()
    }
}

impl Conversation for StandIn {
    fn on_frame(&self, call_id: &str, body: &CallBody, out: &dyn Fn(CallBody)) {
        match body {
            CallBody::Outcome { outcome: Outcome::Answered } => {
                if let Some(msg) = self.message(call_id) {
                    say(1, &format!("{}. {}", msg.title, msg.body), out);
                }
            }
            CallBody::Commit { turn, text } => {
                say(turn + 1, &format!("I heard: {text}. The conversation part comes next."), out);
            }
            _ => {}
        }
    }
}

fn say(reply: u64, text: &str, out: &dyn Fn(CallBody)) {
    for (idx, text) in sentences(text).into_iter().enumerate() {
        out(CallBody::Speak { reply, idx: idx as u32, text });
    }
    out(CallBody::SpeakDone { reply });
    out(CallBody::Play { reply });
}

/// Splits after each `.`, `!` or `?` that is followed by whitespace.
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let ends = matches!(c, '.' | '!' | '?') && chars.peek().is_some_and(|(_, n)| n.is_whitespace());
        if ends {
            out.push(text[start..=i].trim().to_string());
            start = i + 1;
        }
    }
    out.push(text[start..].trim().to_string());
    out.retain(|s| !s.is_empty());
    out
}

#[cfg(test)]
mod tests {
    use super::sentences;

    #[test]
    fn sentences_split_at_end_marks_before_a_space() {
        assert_eq!(sentences("Check-in. how is it going?  Fine!"), ["Check-in.", "how is it going?", "Fine!"]);
        assert_eq!(sentences("v1.2 is out"), ["v1.2 is out"]);
        assert!(sentences("  ").is_empty());
    }
}
