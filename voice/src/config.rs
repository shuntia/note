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
