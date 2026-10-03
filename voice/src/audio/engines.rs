use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{anyhow, Context};
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig, OfflineTtsModelConfig,
    OnlineModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineStream, OnlineTransducerModelConfig,
    SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};

use super::mel::{log_mel, MEL_BINS, MEL_FRAMES};
use crate::config::{ModelSet, ModelsConfig};

pub trait Vad: Send {
    /// One 512-sample (32 ms) window at 16 kHz; returns true while speech is detected.
    fn push(&mut self, window: &[f32]) -> bool;
    fn reset(&mut self);
}

pub trait SpeechToText: Send {
    fn accept(&mut self, samples_16k: &[f32]);
    /// The transcript of the current turn so far, punctuated.
    fn partial(&mut self) -> String;
    /// Ends the current turn: flushes, returns the final text, and starts a fresh stream.
    fn finish(&mut self) -> String;
}

pub trait TurnDetector: Send + Sync {
    /// Probability in [0, 1] that the speaker has finished, over the last 8 s.
    fn complete(&self, samples_16k: &[f32]) -> f32;
}

pub trait TextToSpeech: Send + Sync {
    /// 48 kHz mono i16 for `text` in `voice` (empty = the language default).
    fn synthesize(&self, text: &str, voice: &str) -> anyhow::Result<Vec<i16>>;
    /// `synthesize` at the model's own `NATIVE_RATE`.
    fn synthesize_native(&self, text: &str, voice: &str) -> anyhow::Result<Vec<i16>>;
    fn voices(&self) -> Vec<VoiceInfo>;
}

/// The per-language engines a live call draws on.
pub trait SpeechEngines: Send + Sync {
    fn languages(&self) -> Vec<String>;
    fn vad(&self, language: &str) -> anyhow::Result<Box<dyn Vad>>;
    fn stt(&self, language: &str) -> anyhow::Result<Box<dyn SpeechToText>>;
    /// Panics when no language is loaded.
    fn turn(&self, language: &str) -> Arc<dyn TurnDetector>;
    /// Panics when no language is loaded.
    fn tts(&self, language: &str) -> Arc<dyn TextToSpeech>;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VoiceInfo {
    pub id: String,
    pub label: String,
    pub language: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    #[default]
    Auto,
    Cuda,
    Cpu,
}

const SAMPLE_RATE: i32 = 16_000;
const KOKORO_RATE: i32 = 24_000;
pub const NATIVE_RATE: u32 = KOKORO_RATE as u32;

struct Language {
    vad_model: String,
    recognizer: Arc<OnlineRecognizer>,
    turn: Arc<OrtTurn>,
    tts: Arc<SherpaTts>,
}

pub struct Engines {
    languages: BTreeMap<String, Language>,
}

impl Engines {
    pub fn empty() -> Engines {
        Engines { languages: BTreeMap::new() }
    }

    pub fn load(models: &ModelsConfig, device: Device) -> anyhow::Result<Engines> {
        let mut languages = BTreeMap::new();
        for (code, set) in models {
            let language = Language {
                vad_model: path_str(&set.vad)?,
                recognizer: Arc::new(recognizer(set).with_context(|| format!("loading the {code} recognizer"))?),
                turn: Arc::new(OrtTurn::create(&set.turn).with_context(|| format!("loading the {code} turn model"))?),
                tts: Arc::new(
                    SherpaTts::create(code, set, device, GpuAttempt::Try)
                        .with_context(|| format!("loading the {code} voice"))?,
                ),
            };
            language.tts.synthesize("Hello.", "")?;
            language
                .turn
                .run(&vec![0.0; SAMPLE_RATE as usize])
                .with_context(|| format!("warming the {code} turn model"))?;
            languages.insert(code.clone(), language);
        }
        Ok(Engines { languages })
    }

    /// `language`, or the first loaded one when it is not loaded.
    fn language(&self, language: &str) -> anyhow::Result<&Language> {
        self.languages
            .get(language)
            .or_else(|| self.languages.values().next())
            .ok_or_else(|| anyhow!("no voice models are loaded"))
    }
}

impl SpeechEngines for Engines {
    fn vad(&self, language: &str) -> anyhow::Result<Box<dyn Vad>> {
        Ok(Box::new(SherpaVad::create(&self.language(language)?.vad_model)?))
    }

    fn stt(&self, language: &str) -> anyhow::Result<Box<dyn SpeechToText>> {
        Ok(Box::new(SherpaStt::new(self.language(language)?.recognizer.clone())))
    }

    fn turn(&self, language: &str) -> Arc<dyn TurnDetector> {
        self.language(language).expect("no language loaded").turn.clone()
    }

    fn tts(&self, language: &str) -> Arc<dyn TextToSpeech> {
        self.language(language).expect("no language loaded").tts.clone()
    }

    fn languages(&self) -> Vec<String> {
        self.languages.keys().cloned().collect()
    }
}

/// The Kokoro speaker id for `voice`, or the set's default for an unknown or empty one.
pub fn resolve_sid(set: &ModelSet, voice: &str) -> i32 {
    set.voices.iter().find(|v| v.id == voice).or(set.voices.first()).map_or(0, |v| v.sid)
}

fn path_str(p: &Path) -> anyhow::Result<String> {
    p.to_str().map(str::to_owned).ok_or_else(|| anyhow!("{} is not UTF-8", p.display()))
}

fn recognizer(set: &ModelSet) -> anyhow::Result<OnlineRecognizer> {
    let config = OnlineRecognizerConfig {
        model_config: OnlineModelConfig {
            transducer: OnlineTransducerModelConfig {
                encoder: Some(path_str(&set.stt_encoder)?),
                decoder: Some(path_str(&set.stt_decoder)?),
                joiner: Some(path_str(&set.stt_joiner)?),
            },
            tokens: Some(path_str(&set.stt_tokens)?),
            num_threads: 4,
            provider: Some("cpu".into()),
            ..Default::default()
        },
        decoding_method: Some("greedy_search".into()),
        enable_endpoint: false,
        ..Default::default()
    };
    OnlineRecognizer::create(&config).ok_or_else(|| anyhow!("sherpa-onnx refused the recognizer config"))
}

struct SherpaVad {
    vad: VoiceActivityDetector,
}

impl SherpaVad {
    fn create(model: &str) -> anyhow::Result<Self> {
        let config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(model.into()),
                threshold: 0.5,
                min_silence_duration: 0.1,
                min_speech_duration: 0.1,
                window_size: 512,
                ..Default::default()
            },
            sample_rate: SAMPLE_RATE,
            num_threads: 1,
            provider: Some("cpu".into()),
            ..Default::default()
        };
        let vad = VoiceActivityDetector::create(&config, 30.0).ok_or_else(|| anyhow!("loading the VAD failed"))?;
        Ok(SherpaVad { vad })
    }
}

impl Vad for SherpaVad {
    fn push(&mut self, window: &[f32]) -> bool {
        self.vad.accept_waveform(window);
        while !self.vad.is_empty() {
            self.vad.pop();
        }
        self.vad.detected()
    }

    fn reset(&mut self) {
        self.vad.reset();
    }
}

struct SherpaStt {
    stream: OnlineStream,
    recognizer: Arc<OnlineRecognizer>,
}

impl SherpaStt {
    fn new(recognizer: Arc<OnlineRecognizer>) -> Self {
        SherpaStt { stream: recognizer.create_stream(), recognizer }
    }

    fn decode(&self) {
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
    }

    fn text(&self) -> String {
        self.recognizer.get_result(&self.stream).map(|r| r.text.trim().to_owned()).unwrap_or_default()
    }
}

impl SpeechToText for SherpaStt {
    fn accept(&mut self, samples_16k: &[f32]) {
        self.stream.accept_waveform(SAMPLE_RATE, samples_16k);
        self.decode();
    }

    fn partial(&mut self) -> String {
        self.text()
    }

    fn finish(&mut self) -> String {
        self.stream.accept_waveform(SAMPLE_RATE, &vec![0.0; SAMPLE_RATE as usize * 3 / 10]);
        self.stream.input_finished();
        self.decode();
        let text = self.text();
        self.stream = self.recognizer.create_stream();
        text
    }
}

struct OrtTurn {
    session: Mutex<ort::session::Session>,
    warned: AtomicBool,
}

/// Binds ort to the onnxruntime sherpa-onnx already loaded, so the process holds one;
/// `ORT_DYLIB_PATH` when none is loaded yet.
fn init_ort() -> anyhow::Result<()> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        let path = loaded_onnxruntime()
            .or_else(|| std::env::var_os("ORT_DYLIB_PATH").map(PathBuf::from))
            .ok_or("ORT_DYLIB_PATH is not set")?;
        ort::init_from(path).map_err(|e| e.to_string())?.commit();
        Ok(())
    })
    .clone()
    .map_err(|e| anyhow!("loading onnxruntime: {e}"))
}

fn loaded_onnxruntime() -> Option<PathBuf> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    maps.lines()
        .filter_map(|l| l.split_whitespace().nth(5))
        .find(|p| p.ends_with("/libonnxruntime.so") || p.contains("/libonnxruntime.so."))
        .map(PathBuf::from)
}

impl OrtTurn {
    fn create(model: &Path) -> anyhow::Result<Self> {
        init_ort()?;
        let session = ort::session::Session::builder()?
            .with_intra_threads(1)
            .map_err(|e| anyhow!("{e}"))?
            .commit_from_file(model)?;
        Ok(OrtTurn { session: Mutex::new(session), warned: AtomicBool::new(false) })
    }

    fn run(&self, samples_16k: &[f32]) -> anyhow::Result<f32> {
        let input = ort::value::Tensor::from_array(([1usize, MEL_BINS, MEL_FRAMES], log_mel(samples_16k)))?;
        let mut session = self.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let outputs = session.run(ort::inputs![input])?;
        let (_, logits) = outputs[0].try_extract_tensor::<f32>()?;
        logits.first().copied().ok_or_else(|| anyhow!("Smart Turn returned no logits"))
    }
}

impl TurnDetector for OrtTurn {
    /// A failed pass scores 0, so the turn stays open; only the first failure is logged.
    fn complete(&self, samples_16k: &[f32]) -> f32 {
        match self.run(samples_16k) {
            Ok(p) => p.clamp(0.0, 1.0),
            Err(e) => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    eprintln!("voice: Smart Turn failed: {e:#}");
                }
                0.0
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GpuAttempt {
    Try,
    /// Treats the GPU engine as unavailable without asking sherpa-onnx for it.
    #[cfg(test)]
    Fail,
}

struct SherpaTts {
    tts: OfflineTts,
    #[cfg_attr(not(test), allow(dead_code))]
    provider: &'static str,
    set: ModelSet,
    language: String,
}

impl SherpaTts {
    fn create(language: &str, set: &ModelSet, device: Device, gpu: GpuAttempt) -> anyhow::Result<Self> {
        let config = |provider: &str| -> anyhow::Result<OfflineTtsConfig> {
            Ok(OfflineTtsConfig {
                model: OfflineTtsModelConfig {
                    kokoro: OfflineTtsKokoroModelConfig {
                        model: Some(path_str(&set.tts_model)?),
                        voices: Some(path_str(&set.tts_voices)?),
                        tokens: Some(path_str(&set.tts_tokens)?),
                        data_dir: Some(path_str(&set.tts_data_dir)?),
                        ..Default::default()
                    },
                    num_threads: 2,
                    provider: Some(provider.into()),
                    ..Default::default()
                },
                ..Default::default()
            })
        };
        let mut cuda = None;
        if device != Device::Cpu {
            if gpu == GpuAttempt::Try {
                cuda = OfflineTts::create(&config("cuda")?);
            }
            if cuda.is_none() {
                eprintln!("voice: CUDA TTS unavailable, using the CPU");
            }
        }
        let (tts, provider) = match cuda {
            Some(t) => (t, "cuda"),
            None => (
                OfflineTts::create(&config("cpu")?).ok_or_else(|| anyhow!("sherpa-onnx refused the TTS config"))?,
                "cpu",
            ),
        };
        if tts.sample_rate() != KOKORO_RATE {
            anyhow::bail!("expected {KOKORO_RATE} Hz from the TTS, got {}", tts.sample_rate());
        }
        Ok(SherpaTts { tts, provider, set: set.clone(), language: language.into() })
    }
}

impl SherpaTts {
    #[cfg(test)]
    pub(crate) fn provider(&self) -> &'static str {
        self.provider
    }
}

impl SherpaTts {
    fn generate(&self, text: &str, voice: &str) -> anyhow::Result<Vec<f32>> {
        let config = GenerationConfig { sid: resolve_sid(&self.set, voice), ..Default::default() };
        let audio = self
            .tts
            .generate_with_config::<fn(&[f32], f32) -> bool>(text, &config, None)
            .ok_or_else(|| anyhow!("synthesizing {text:?} failed"))?;
        Ok(audio.samples().to_vec())
    }
}

impl TextToSpeech for SherpaTts {
    fn synthesize(&self, text: &str, voice: &str) -> anyhow::Result<Vec<i16>> {
        Ok(to_48k_i16(&self.generate(text, voice)?))
    }

    fn synthesize_native(&self, text: &str, voice: &str) -> anyhow::Result<Vec<i16>> {
        Ok(self.generate(text, voice)?.into_iter().map(pcm).collect())
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        self.set
            .voices
            .iter()
            .map(|v| VoiceInfo { id: v.id.clone(), label: v.label.clone(), language: self.language.clone() })
            .collect()
    }
}

fn pcm(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// 24 kHz float to 48 kHz i16: each sample, then the midpoint to the next.
fn to_48k_i16(samples_24k: &[f32]) -> Vec<i16> {
    let mut out = Vec::with_capacity(samples_24k.len() * 2);
    for (i, &s) in samples_24k.iter().enumerate() {
        let next = samples_24k.get(i + 1).copied().unwrap_or(s);
        out.push(pcm(s));
        out.push(pcm(f32::midpoint(s, next)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{models_from_dir, VoiceEntry};

    fn models() -> Option<ModelsConfig> {
        std::env::var_os("NOTE_VOICE_MODELS").map(|d| models_from_dir(Path::new(&d)))
    }

    fn test_set() -> ModelSet {
        let mut set = models_from_dir(Path::new("/nonexistent")).remove("en").unwrap();
        set.voices = vec![
            VoiceEntry { id: "af_sarah".into(), sid: 3, label: "Sarah".into() },
            VoiceEntry { id: "bm_george".into(), sid: 9, label: "George".into() },
        ];
        set
    }

    #[test]
    #[ignore = "needs NOTE_VOICE_MODELS"]
    fn tts_then_stt_round_trips_a_sentence() {
        let e = Engines::load(&models().unwrap(), Device::Auto).unwrap();
        let pcm48 = e.tts("en").synthesize("Move my run to tomorrow at seven.", "").unwrap();
        let pcm16: Vec<f32> = pcm48.iter().step_by(3).map(|s| f32::from(*s) / 32768.0).collect();
        let mut stt = e.stt("en").unwrap();
        for c in pcm16.chunks(2560) {
            stt.accept(c);
        }
        let text = stt.finish().to_lowercase();
        assert!(text.contains("run") && text.contains("tomorrow"), "{text}");
    }

    #[test]
    #[ignore = "needs NOTE_VOICE_MODELS"]
    fn a_failed_cuda_engine_falls_back_to_cpu() {
        let mut m = models().unwrap();
        let tts = SherpaTts::create("en", &m["en"], Device::Cuda, GpuAttempt::Fail).unwrap();
        assert_eq!(tts.provider(), "cpu");
        assert!(!tts.synthesize("Hi.", "").unwrap().is_empty());
        m.clear();
        assert!(Engines::load(&m, Device::Auto).unwrap().languages().is_empty());
    }

    #[test]
    #[ignore = "needs NOTE_VOICE_MODELS"]
    fn smart_turn_scores_a_finished_sentence_above_a_cut_one() {
        let e = Engines::load(&models().unwrap(), Device::Cpu).unwrap();
        let tts = e.tts("en");
        let down = |p: Vec<i16>| p.iter().step_by(3).map(|s| f32::from(*s) / 32768.0).collect::<Vec<f32>>();
        let done = down(tts.synthesize("Can you move my run to tomorrow?", "").unwrap());
        let cut = down(tts.synthesize("Can you move my", "").unwrap());
        let t = e.turn("en");
        assert!(t.complete(&done) > t.complete(&cut));
    }

    #[test]
    fn kokoro_audio_doubles_to_48k_with_midpoints_and_clamps() {
        assert_eq!(to_48k_i16(&[0.0, 0.5, 2.0]), vec![0, 8192, 16384, 32767, 32767, 32767]);
    }

    #[test]
    fn an_unknown_voice_falls_back_to_the_language_default() {
        let set = test_set();
        assert_eq!(resolve_sid(&set, "bm_george"), 9);
        assert_eq!(resolve_sid(&set, "nope"), set.voices[0].sid);
        assert_eq!(resolve_sid(&set, ""), set.voices[0].sid);
    }
}
