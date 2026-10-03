use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use crate::audio::engines::Device;

#[derive(Debug, Clone, Deserialize)]
pub struct VoiceServiceConfig {
    pub homeserver: String,
    /// The bot's access token, alone in the file.
    pub token_file: PathBuf,
    pub livekit_service_url: String,
    pub socket: PathBuf,
    pub state_dir: PathBuf,
    #[serde(default = "models_dir_from_env")]
    pub models_dir: Option<PathBuf>,
    /// Per-language sets that replace the ones derived from `models_dir`.
    #[serde(default)]
    pub models: ModelsConfig,
    #[serde(default)]
    pub device: Device,
    #[serde(default = "cues_dir_from_env")]
    pub cues_dir: Option<PathBuf>,
    pub ready_cue_file: Option<PathBuf>,
    pub heard_cue_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelSet {
    pub stt_encoder: PathBuf,
    pub stt_decoder: PathBuf,
    pub stt_joiner: PathBuf,
    pub stt_tokens: PathBuf,
    pub tts_model: PathBuf,
    pub tts_voices: PathBuf,
    pub tts_tokens: PathBuf,
    pub tts_data_dir: PathBuf,
    pub vad: PathBuf,
    pub turn: PathBuf,
    /// Kokoro speaker ids offered, with labels; the first is the default.
    pub voices: Vec<VoiceEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VoiceEntry {
    pub id: String,
    pub sid: i32,
    pub label: String,
}

pub type ModelsConfig = BTreeMap<String, ModelSet>;

const KOKORO_EN_VOICES: [(&str, i32, &str); 11] = [
    ("af_sarah", 3, "Sarah"),
    ("af", 0, "Bella & Sarah"),
    ("af_bella", 1, "Bella"),
    ("af_nicole", 2, "Nicole"),
    ("af_sky", 4, "Sky"),
    ("am_adam", 5, "Adam"),
    ("am_michael", 6, "Michael"),
    ("bf_emma", 7, "Emma"),
    ("bf_isabella", 8, "Isabella"),
    ("bm_george", 9, "George"),
    ("bm_lewis", 10, "Lewis"),
];

/// The "en" set, laid out as `packages.note-voice-models`.
pub fn models_from_dir(dir: &Path) -> ModelsConfig {
    let en = ModelSet {
        stt_encoder: dir.join("nemotron/encoder.int8.onnx"),
        stt_decoder: dir.join("nemotron/decoder.int8.onnx"),
        stt_joiner: dir.join("nemotron/joiner.int8.onnx"),
        stt_tokens: dir.join("nemotron/tokens.txt"),
        tts_model: dir.join("kokoro/model.onnx"),
        tts_voices: dir.join("kokoro/voices.bin"),
        tts_tokens: dir.join("kokoro/tokens.txt"),
        tts_data_dir: dir.join("kokoro/espeak-ng-data"),
        vad: dir.join("silero_vad.onnx"),
        turn: dir.join("smart-turn.onnx"),
        voices: KOKORO_EN_VOICES
            .iter()
            .map(|(id, sid, label)| VoiceEntry { id: (*id).into(), sid: *sid, label: (*label).into() })
            .collect(),
    };
    BTreeMap::from([("en".to_string(), en)])
}

fn models_dir_from_env() -> Option<PathBuf> {
    std::env::var_os("NOTE_VOICE_MODELS").map(PathBuf::from)
}

fn cues_dir_from_env() -> Option<PathBuf> {
    std::env::var_os("NOTE_VOICE_CUES").map(PathBuf::from)
}

impl VoiceServiceConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        Ok(toml::from_str(&raw)?)
    }

    /// `models` when set, else the sets found in `models_dir`, else none.
    pub fn model_sets(&self) -> ModelsConfig {
        if !self.models.is_empty() {
            return self.models.clone();
        }
        self.models_dir.as_deref().map(models_from_dir).unwrap_or_default()
    }

    pub fn ready_cue(&self) -> Option<PathBuf> {
        self.cue(self.ready_cue_file.as_ref(), "ready.pcm")
    }

    pub fn heard_cue(&self) -> Option<PathBuf> {
        self.cue(self.heard_cue_file.as_ref(), "heard.pcm")
    }

    fn cue(&self, file: Option<&PathBuf>, name: &str) -> Option<PathBuf> {
        file.cloned().or_else(|| self.cues_dir.as_ref().map(|d| d.join(name)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_and_cues_come_from_their_dirs_unless_overridden() {
        let cfg: VoiceServiceConfig = toml::from_str(
            r#"
            homeserver = "https://hs.t"
            token_file = "/t"
            livekit_service_url = "https://rtc.t"
            socket = "/s"
            state_dir = "/st"
            models_dir = "/m"
            cues_dir = "/c"
            heard_cue_file = "/h.pcm"
            device = "cpu"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.device, Device::Cpu);
        assert_eq!(cfg.model_sets()["en"].turn, Path::new("/m/smart-turn.onnx"));
        assert_eq!(cfg.model_sets()["en"].voices[0].id, "af_sarah");
        assert_eq!(cfg.ready_cue(), Some(PathBuf::from("/c/ready.pcm")));
        assert_eq!(cfg.heard_cue(), Some(PathBuf::from("/h.pcm")));
    }
}
