use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use crate::audio::engines::Device;
pub use crate::audio::sidecar::SidecarConfig;

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
    #[serde(default)]
    pub tts: TtsConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TtsConfig {
    /// Speech models run as their own processes, offered beside Kokoro while they answer.
    #[serde(default)]
    pub sidecars: Vec<SidecarConfig>,
}

impl TtsConfig {
    /// A sidecar id prefixes its voice ids, so it must be unique, free of ':', and not Kokoro's.
    pub fn check(&self) -> anyhow::Result<()> {
        for (i, sidecar) in self.sidecars.iter().enumerate() {
            let id = &sidecar.id;
            if id.is_empty() || id.contains(':') {
                anyhow::bail!("tts.sidecars: the id {id:?} must be non-empty and contain no ':'");
            }
            if id == crate::audio::engines::KOKORO {
                anyhow::bail!("tts.sidecars: the id {id:?} is Kokoro's");
            }
            if self.sidecars[..i].iter().any(|s| s.id == *id) {
                anyhow::bail!("tts.sidecars: the id {id:?} is used twice");
            }
        }
        Ok(())
    }
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
    /// Kokoro v1 pronunciation lexicons, comma-separated in priority order.
    #[serde(default)]
    pub tts_lexicon: Option<String>,
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

/// Kokoro v1.0's English speakers (ids 0-27 of 53); the first is the default.
const KOKORO_EN_VOICES: [(&str, i32, &str); 28] = [
    ("af_heart", 3, "Heart"),
    ("af_alloy", 0, "Alloy"),
    ("af_aoede", 1, "Aoede"),
    ("af_bella", 2, "Bella"),
    ("af_jessica", 4, "Jessica"),
    ("af_kore", 5, "Kore"),
    ("af_nicole", 6, "Nicole"),
    ("af_nova", 7, "Nova"),
    ("af_river", 8, "River"),
    ("af_sarah", 9, "Sarah"),
    ("af_sky", 10, "Sky"),
    ("am_adam", 11, "Adam"),
    ("am_echo", 12, "Echo"),
    ("am_eric", 13, "Eric"),
    ("am_fenrir", 14, "Fenrir"),
    ("am_liam", 15, "Liam"),
    ("am_michael", 16, "Michael"),
    ("am_onyx", 17, "Onyx"),
    ("am_puck", 18, "Puck"),
    ("am_santa", 19, "Santa"),
    ("bf_alice", 20, "Alice"),
    ("bf_emma", 21, "Emma"),
    ("bf_isabella", 22, "Isabella"),
    ("bf_lily", 23, "Lily"),
    ("bm_daniel", 24, "Daniel"),
    ("bm_fable", 25, "Fable"),
    ("bm_george", 26, "George"),
    ("bm_lewis", 27, "Lewis"),
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
        tts_lexicon: Some(format!(
            "{},{}",
            dir.join("kokoro/lexicon-us-en.txt").display(),
            dir.join("kokoro/lexicon-gb-en.txt").display()
        )),
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
        let cfg: Self = toml::from_str(&raw)?;
        cfg.tts.check()?;
        Ok(cfg)
    }

    /// `models` when set, else the sets found in `models_dir`, else none.
    pub fn model_sets(&self) -> ModelsConfig {
        if !self.models.is_empty() {
            return self.models.clone();
        }
        self.models_dir.as_deref().map(models_from_dir).unwrap_or_default()
    }

    /// The override, then the default in `cues_dir`, in the order to try them.
    pub fn ready_cue(&self) -> Vec<PathBuf> {
        self.cue(self.ready_cue_file.as_ref(), "ready.pcm")
    }

    pub fn heard_cue(&self) -> Vec<PathBuf> {
        self.cue(self.heard_cue_file.as_ref(), "heard.pcm")
    }

    fn cue(&self, file: Option<&PathBuf>, name: &str) -> Vec<PathBuf> {
        file.cloned().into_iter().chain(self.cues_dir.as_ref().map(|d| d.join(name))).collect()
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

            [[tts.sidecars]]
            id = "kyutai"
            url = "http://127.0.0.1:8890"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.device, Device::Cpu);
        assert_eq!(cfg.model_sets()["en"].turn, Path::new("/m/smart-turn.onnx"));
        assert_eq!(cfg.model_sets()["en"].voices[0].id, "af_heart");
        assert_eq!(cfg.ready_cue(), vec![PathBuf::from("/c/ready.pcm")]);
        assert_eq!(cfg.heard_cue(), vec![PathBuf::from("/h.pcm"), PathBuf::from("/c/heard.pcm")]);
        assert_eq!(cfg.tts.sidecars, [SidecarConfig { id: "kyutai".into(), url: "http://127.0.0.1:8890".into() }]);
        assert!(cfg.tts.check().is_ok());
    }

    #[test]
    fn a_sidecar_id_that_would_shadow_a_voice_is_refused() {
        let tts = |ids: &[&str]| TtsConfig {
            sidecars: ids.iter().map(|id| SidecarConfig { id: (*id).into(), url: "http://127.0.0.1:1".into() }).collect(),
        };
        for ids in [&["a:b"][..], &["kokoro"], &["x", "x"], &[""]] {
            let refused = tts(ids).check().unwrap_err().to_string();
            assert!(refused.starts_with("tts.sidecars: the id"), "{refused}");
        }
        assert!(tts(&["kyutai", "chatterbox"]).check().is_ok());
    }
}
