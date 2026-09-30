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
