use super::OutboundMessage;
use crate::voice::{links, Voice};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

pub struct VoicePhone {
    voice: Arc<Voice>,
    db: Arc<Mutex<Connection>>,
}

impl VoicePhone {
    pub fn new(voice: Arc<Voice>, db: Arc<Mutex<Connection>>) -> Self {
        Self { voice, db }
    }
}

impl super::calling::PhoneRing for VoicePhone {
    fn ring_phone(&self, user_id: i64, msg: &OutboundMessage) -> anyhow::Result<String> {
        let link = {
            let conn = crate::db_guard(&self.db);
            links::ringable(&conn, user_id)?
        }
        .ok_or(super::calling::NoLinkedPhone)?;
        anyhow::ensure!(self.voice.is_up(), "the voice service is not connected");
        self.voice.start_call(user_id, &link, msg, jiff::Timestamp::now())
    }
}
