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
    /// A session is running for the answered call.
    #[serde(default)]
    pub live: bool,
    /// The seq of this side's `Ended` frame, once sent.
    pub ended_seq: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub links: BTreeMap<i64, LinkState>,
    pub calls: BTreeMap<String, CallState>,
    pub since: Option<String>,
    /// Each linked account's profile as of its last call, for what is said when Note cannot be reached.
    #[serde(default)]
    pub profiles: BTreeMap<String, note_voice_proto::VoiceProfile>,
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
