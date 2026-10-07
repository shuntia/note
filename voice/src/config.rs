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
    /// Replaces the identifier found in `models_dir`.
    #[serde(default)]
    pub language_id: Option<LanguageIdModel>,
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
    /// The sidecar that is a language's base voice when `models_dir` holds no Kokoro for it, by
    /// language; a language left out uses the sidecar named like it.
    #[serde(default)]
    pub base: BTreeMap<String, String>,
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

    /// The sidecar id that is `language`'s base voice.
    pub fn base_sidecar(&self, language: &str) -> String {
        self.base.get(language).cloned().unwrap_or_else(|| language.to_owned())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SttModel {
    /// A streaming zipformer, decoded as the audio comes.
    OnlineTransducer { encoder: PathBuf, decoder: PathBuf, joiner: PathBuf, tokens: PathBuf },
    /// A whole-utterance zipformer; the turn so far is re-decoded at a throttled cadence.
    OfflineTransducer { encoder: PathBuf, decoder: PathBuf, joiner: PathBuf, tokens: PathBuf },
}

#[derive(Debug, Clone, Deserialize)]
pub struct KokoroModel {
    pub model: PathBuf,
    pub voices: PathBuf,
    pub tokens: PathBuf,
    pub data_dir: PathBuf,
    /// Kokoro v1 pronunciation lexicons, comma-separated in priority order.
    #[serde(default)]
    pub lexicon: Option<String>,
    /// The speaker ids offered, with labels; the first is the default.
    pub speakers: Vec<VoiceEntry>,
}

/// A language's models. Its base voice is `kokoro` when set, else the `tts_sidecar` named.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelSet {
    pub stt: SttModel,
    #[serde(default)]
    pub kokoro: Option<KokoroModel>,
    #[serde(default)]
    pub tts_sidecar: Option<String>,
    pub vad: PathBuf,
    pub turn: PathBuf,
}

impl ModelSet {
    fn check(&self, code: &str, tts: &TtsConfig) -> anyhow::Result<()> {
        match (&self.kokoro, &self.tts_sidecar) {
            (None, None) => anyhow::bail!("models.{code}: neither kokoro nor tts_sidecar is set"),
            (Some(_), Some(_)) => anyhow::bail!("models.{code}: both kokoro and tts_sidecar are set"),
            (None, Some(id)) if !tts.sidecars.iter().any(|s| s.id == *id) => {
                anyhow::bail!("models.{code}: tts_sidecar {id:?} is not in tts.sidecars")
            }
            _ => Ok(()),
        }
    }
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

pub const REAZON_DIR: &str = "ja/reazonspeech";
pub const WHISPER_DIR: &str = "whisper-tiny";

/// Whisper's encoder and decoder, as sherpa-onnx exports them, for telling a caller's language.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LanguageIdModel {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
}

/// The identifier laid out as `packages.note-voice-models`, when it is there.
pub fn language_id_from_dir(dir: &Path) -> Option<LanguageIdModel> {
    let whisper = dir.join(WHISPER_DIR);
    let model = LanguageIdModel { encoder: whisper.join("tiny-encoder.int8.onnx"), decoder: whisper.join("tiny-decoder.int8.onnx") };
    (model.encoder.is_file() && model.decoder.is_file()).then_some(model)
}

/// The sets laid out as `packages.note-voice-models`: "en" always, "ja" when its models are there,
/// speaking through `tts.base_sidecar("ja")`.
pub fn models_from_dir(dir: &Path, tts: &TtsConfig) -> ModelsConfig {
    let en = ModelSet {
        stt: SttModel::OnlineTransducer {
            encoder: dir.join("nemotron/encoder.int8.onnx"),
            decoder: dir.join("nemotron/decoder.int8.onnx"),
            joiner: dir.join("nemotron/joiner.int8.onnx"),
            tokens: dir.join("nemotron/tokens.txt"),
        },
        kokoro: Some(KokoroModel {
            model: dir.join("kokoro/model.onnx"),
            voices: dir.join("kokoro/voices.bin"),
            tokens: dir.join("kokoro/tokens.txt"),
            data_dir: dir.join("kokoro/espeak-ng-data"),
            lexicon: Some(format!(
                "{},{}",
                dir.join("kokoro/lexicon-us-en.txt").display(),
                dir.join("kokoro/lexicon-gb-en.txt").display()
            )),
            speakers: KOKORO_EN_VOICES
                .iter()
                .map(|(id, sid, label)| VoiceEntry { id: (*id).into(), sid: *sid, label: (*label).into() })
                .collect(),
        }),
        tts_sidecar: None,
        vad: dir.join("silero_vad.onnx"),
        turn: dir.join("smart-turn.onnx"),
    };
    let mut sets = BTreeMap::from([("en".to_string(), en)]);
    let reazon = dir.join(REAZON_DIR);
    if reazon.join("tokens.txt").is_file() {
        let ja = ModelSet {
            stt: SttModel::OfflineTransducer {
                encoder: reazon.join("encoder-epoch-99-avg-1.int8.onnx"),
                decoder: reazon.join("decoder-epoch-99-avg-1.int8.onnx"),
                joiner: reazon.join("joiner-epoch-99-avg-1.int8.onnx"),
                tokens: reazon.join("tokens.txt"),
            },
            kokoro: None,
            tts_sidecar: Some(tts.base_sidecar("ja")),
            vad: dir.join("silero_vad.onnx"),
            turn: dir.join("smart-turn.onnx"),
        };
        sets.insert("ja".to_string(), ja);
    }
    sets
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
        cfg.check()?;
        Ok(cfg)
    }

    pub fn check(&self) -> anyhow::Result<()> {
        self.tts.check()?;
        for (code, set) in &self.models {
            set.check(code, &self.tts)?;
        }
        Ok(())
    }

    /// `models` when set, else the sets found in `models_dir`, else none. A set whose base is a
    /// sidecar not configured is left out, so a missing sidecar costs only its language.
    pub fn model_sets(&self) -> ModelsConfig {
        let mut sets = if self.models.is_empty() {
            self.models_dir.as_deref().map(|dir| models_from_dir(dir, &self.tts)).unwrap_or_default()
        } else {
            self.models.clone()
        };
        sets.retain(|code, set| match set.check(code, &self.tts) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("voice: {e:#}; {code} is left out");
                false
            }
        });
        sets
    }

    pub fn language_id(&self) -> Option<LanguageIdModel> {
        self.language_id.clone().or_else(|| self.models_dir.as_deref().and_then(language_id_from_dir))
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

    const BASE: &str = r#"
        homeserver = "https://hs.t"
        token_file = "/t"
        livekit_service_url = "https://rtc.t"
        socket = "/s"
        state_dir = "/st"
        models_dir = "/m"
        cues_dir = "/c"
        heard_cue_file = "/h.pcm"
        device = "cpu"
    "#;

    #[test]
    fn models_and_cues_come_from_their_dirs_unless_overridden() {
        let cfg: VoiceServiceConfig = toml::from_str(&format!(
            r#"{BASE}
            [[tts.sidecars]]
            id = "kyutai"
            url = "http://127.0.0.1:8890"
            "#
        ))
        .unwrap();
        assert_eq!(cfg.device, Device::Cpu);
        let sets = cfg.model_sets();
        assert_eq!(sets["en"].turn, Path::new("/m/smart-turn.onnx"));
        assert_eq!(sets["en"].kokoro.as_ref().unwrap().speakers[0].id, "af_heart");
        assert!(matches!(sets["en"].stt, SttModel::OnlineTransducer { .. }));
        assert!(!sets.contains_key("ja"), "no Japanese models under /m");
        assert_eq!(cfg.ready_cue(), vec![PathBuf::from("/c/ready.pcm")]);
        assert_eq!(cfg.heard_cue(), vec![PathBuf::from("/h.pcm"), PathBuf::from("/c/heard.pcm")]);
        assert_eq!(cfg.tts.sidecars, [SidecarConfig { id: "kyutai".into(), url: "http://127.0.0.1:8890".into() }]);
        assert!(cfg.check().is_ok());
    }

    #[test]
    fn a_japanese_set_appears_with_its_models_and_speaks_through_its_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let reazon = dir.path().join(REAZON_DIR);
        std::fs::create_dir_all(&reazon).unwrap();
        std::fs::write(reazon.join("tokens.txt"), "<blk>\n").unwrap();
        let tts = TtsConfig {
            sidecars: vec![SidecarConfig { id: "ja-tts".into(), url: "http://127.0.0.1:1".into() }],
            base: BTreeMap::from([("ja".to_string(), "ja-tts".to_string())]),
        };
        let sets = models_from_dir(dir.path(), &tts);
        let ja = &sets["ja"];
        assert!(matches!(&ja.stt, SttModel::OfflineTransducer { tokens, .. } if tokens == &reazon.join("tokens.txt")));
        assert!(ja.kokoro.is_none());
        assert_eq!(ja.tts_sidecar.as_deref(), Some("ja-tts"));
        assert_eq!(ja.vad, sets["en"].vad);
        assert_eq!(models_from_dir(dir.path(), &TtsConfig::default())["ja"].tts_sidecar.as_deref(), Some("ja"));
    }

    #[test]
    fn the_language_identifier_comes_from_the_models_dir_when_it_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: VoiceServiceConfig = toml::from_str(BASE).unwrap();
        cfg.models_dir = Some(dir.path().to_path_buf());
        assert_eq!(cfg.language_id(), None);
        let whisper = dir.path().join(WHISPER_DIR);
        std::fs::create_dir_all(&whisper).unwrap();
        for f in ["tiny-encoder.int8.onnx", "tiny-decoder.int8.onnx"] {
            std::fs::write(whisper.join(f), "").unwrap();
        }
        assert_eq!(cfg.language_id().unwrap().encoder, whisper.join("tiny-encoder.int8.onnx"));
        let own: VoiceServiceConfig =
            toml::from_str(&format!("{BASE}\nlanguage_id = {{ encoder = \"/e\", decoder = \"/d\" }}")).unwrap();
        assert_eq!(own.language_id(), Some(LanguageIdModel { encoder: "/e".into(), decoder: "/d".into() }));
    }

    #[test]
    fn a_set_whose_sidecar_is_not_configured_is_left_out_of_the_loaded_ones() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(REAZON_DIR)).unwrap();
        std::fs::write(dir.path().join(REAZON_DIR).join("tokens.txt"), "").unwrap();
        let mut cfg: VoiceServiceConfig = toml::from_str(BASE).unwrap();
        cfg.models_dir = Some(dir.path().to_path_buf());
        assert_eq!(cfg.model_sets().keys().collect::<Vec<_>>(), ["en"]);
        cfg.tts.sidecars.push(SidecarConfig { id: "ja".into(), url: "http://127.0.0.1:1".into() });
        assert_eq!(cfg.model_sets().keys().collect::<Vec<_>>(), ["en", "ja"]);
    }

    #[test]
    fn an_explicit_set_needs_exactly_one_base_voice() {
        let set = |body: &str| -> anyhow::Result<()> {
            let cfg: VoiceServiceConfig = toml::from_str(&format!(
                r#"{BASE}
                [[tts.sidecars]]
                id = "ja"
                url = "http://127.0.0.1:1"

                [models.ja]
                stt = {{ kind = "offline_transducer", encoder = "/e", decoder = "/d", joiner = "/j", tokens = "/t" }}
                vad = "/v"
                turn = "/s"
                {body}
                "#
            ))?;
            cfg.check()
        };
        assert!(set(r#"tts_sidecar = "ja""#).is_ok());
        let refused = |body| set(body).unwrap_err().to_string();
        assert!(refused("").contains("neither"), "{}", refused(""));
        assert!(refused(r#"tts_sidecar = "nope""#).contains("not in tts.sidecars"));
        let both = r#"tts_sidecar = "ja"
            kokoro = { model = "/m", voices = "/v", tokens = "/t", data_dir = "/d", speakers = [] }"#;
        assert!(refused(both).contains("both"));
    }

    #[test]
    fn a_sidecar_id_that_would_shadow_a_voice_is_refused() {
        let tts = |ids: &[&str]| TtsConfig {
            sidecars: ids.iter().map(|id| SidecarConfig { id: (*id).into(), url: "http://127.0.0.1:1".into() }).collect(),
            base: BTreeMap::new(),
        };
        for ids in [&["a:b"][..], &["kokoro"], &["x", "x"], &[""]] {
            let refused = tts(ids).check().unwrap_err().to_string();
            assert!(refused.starts_with("tts.sidecars: the id"), "{refused}");
        }
        assert!(tts(&["kyutai", "chatterbox"]).check().is_ok());
    }
}
