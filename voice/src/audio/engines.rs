use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{anyhow, Context};
use sherpa_onnx::{
    GenerationConfig, OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
    OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig, OfflineTtsModelConfig, OnlineModelConfig,
    OnlineRecognizer, OnlineRecognizerConfig, OnlineStream, OnlineTransducerModelConfig, SileroVadModelConfig,
    VadModelConfig, VoiceActivityDetector,
};

use super::language::{LanguageId, Scores, WhisperLanguageId};
use super::mel::{log_mel, MEL_BINS, MEL_FRAMES};
use super::stt::{Decoder, OfflineStt};
use super::tts::{ChunkedBackend, Renderer, SpeechBackend};
use crate::config::{KokoroModel, LanguageIdModel, ModelSet, ModelsConfig, SttModel};

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
    /// Discards the current turn.
    fn reset(&mut self) {
        self.finish();
    }
}

pub trait TurnDetector: Send + Sync {
    /// Probability in [0, 1] that the speaker has finished, over the last 8 s.
    fn complete(&self, samples_16k: &[f32]) -> f32;
}

/// A language's base voice: what its canned lines, previews and fallbacks speak in.
#[derive(Clone)]
pub enum BaseVoice {
    Kokoro(Arc<dyn SpeechBackend>),
    /// A sidecar by id, up or not.
    Sidecar(String),
}

impl BaseVoice {
    pub fn id(&self) -> &str {
        match self {
            BaseVoice::Kokoro(kokoro) => kokoro.id(),
            BaseVoice::Sidecar(id) => id,
        }
    }

    /// The backend, if it is up now.
    pub fn live(&self, sidecars: &[Arc<dyn SpeechBackend>]) -> Option<Arc<dyn SpeechBackend>> {
        match self {
            BaseVoice::Kokoro(kokoro) => Some(kokoro.clone()),
            BaseVoice::Sidecar(id) => sidecars.iter().find(|s| s.id() == id).cloned(),
        }
    }
}

/// The per-language engines a live call draws on.
pub trait SpeechEngines: Send + Sync {
    fn languages(&self) -> Vec<String>;
    fn vad(&self, language: &str) -> anyhow::Result<Box<dyn Vad>>;
    fn stt(&self, language: &str) -> anyhow::Result<Box<dyn SpeechToText>>;
    /// Panics when no language is loaded.
    fn turn(&self, language: &str) -> Arc<dyn TurnDetector>;
    /// The base voice of `language`. Panics when no language is loaded.
    fn tts(&self, language: &str) -> BaseVoice;
    fn identifies(&self) -> bool {
        false
    }
    /// Tells the spoken language of a clip; `None` without an identifier loaded.
    fn identify(&self, _samples_16k: &[f32]) -> Option<anyhow::Result<Scores>> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VoiceInfo {
    pub id: String,
    pub label: String,
    /// The languages it speaks; empty for any.
    pub languages: Vec<String>,
    /// The attribution its licence asks to be shown with it.
    pub credit: Option<String>,
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

enum Recognizer {
    Online(Arc<OnlineRecognizer>),
    Offline(Arc<OfflineRecognizer>),
}

struct Language {
    vad_model: String,
    recognizer: Recognizer,
    turn: Arc<OrtTurn>,
    tts: BaseVoice,
}

pub struct Engines {
    languages: BTreeMap<String, Language>,
    identifier: Option<Arc<dyn LanguageId>>,
}

impl Engines {
    pub fn empty() -> Engines {
        Engines { languages: BTreeMap::new(), identifier: None }
    }

    /// Loads the spoken language identifier; one that fails is logged and calls keep the user's language.
    #[must_use]
    pub fn with_identifier(mut self, model: Option<&LanguageIdModel>) -> Engines {
        self.identifier = model.and_then(|m| match WhisperLanguageId::create(&m.encoder, &m.decoder) {
            Ok(id) => Some(Arc::new(id) as Arc<dyn LanguageId>),
            Err(e) => {
                eprintln!("voice: loading the language identifier failed: {e:#}; calls keep the user's language");
                None
            }
        });
        self
    }

    /// Loads each set; a language that fails is logged and left out, and none loading is an error.
    pub fn load(models: &ModelsConfig, device: Device) -> anyhow::Result<Engines> {
        let mut languages = BTreeMap::new();
        let mut failed = Vec::new();
        for (code, set) in models {
            match Language::load(code, set, device) {
                Ok(language) => {
                    languages.insert(code.clone(), language);
                }
                Err(e) => {
                    eprintln!("voice: loading {code} failed: {e:#}");
                    failed.push(code.as_str());
                }
            }
        }
        if languages.is_empty() && !failed.is_empty() {
            anyhow::bail!("no language loaded ({})", failed.join(", "));
        }
        Ok(Engines { languages, identifier: None })
    }

    /// `language`, or the first loaded one when it is not loaded.
    fn language(&self, language: &str) -> anyhow::Result<&Language> {
        self.languages
            .get(language)
            .or_else(|| self.languages.values().next())
            .ok_or_else(|| anyhow!("no voice models are loaded"))
    }
}

impl Language {
    fn load(code: &str, set: &ModelSet, device: Device) -> anyhow::Result<Language> {
        let tts = match (&set.kokoro, &set.tts_sidecar) {
            (Some(kokoro), _) => {
                let kokoro = SherpaTts::create(code, kokoro, device, GpuAttempt::Try).context("loading the voice")?;
                kokoro.render("Hello.", "")?;
                let voices = kokoro.voices();
                BaseVoice::Kokoro(Arc::new(ChunkedBackend::new(KOKORO, "Kokoro", Arc::new(kokoro), voices)))
            }
            (None, Some(sidecar)) => BaseVoice::Sidecar(sidecar.clone()),
            (None, None) => anyhow::bail!("no base voice"),
        };
        let language = Language {
            vad_model: path_str(&set.vad)?,
            recognizer: recognizer(&set.stt).context("loading the recognizer")?,
            turn: Arc::new(OrtTurn::create(&set.turn).context("loading the turn model")?),
            tts,
        };
        language.turn.run(&vec![0.0; SAMPLE_RATE as usize]).context("warming the turn model")?;
        Ok(language)
    }
}

impl SpeechEngines for Engines {
    fn vad(&self, language: &str) -> anyhow::Result<Box<dyn Vad>> {
        Ok(Box::new(SherpaVad::create(&self.language(language)?.vad_model)?))
    }

    fn stt(&self, language: &str) -> anyhow::Result<Box<dyn SpeechToText>> {
        Ok(match &self.language(language)?.recognizer {
            Recognizer::Online(recognizer) => Box::new(SherpaStt::new(recognizer.clone())),
            Recognizer::Offline(recognizer) => Box::new(OfflineStt::new(recognizer.clone())),
        })
    }

    fn turn(&self, language: &str) -> Arc<dyn TurnDetector> {
        self.language(language).expect("no language loaded").turn.clone()
    }

    fn tts(&self, language: &str) -> BaseVoice {
        self.language(language).expect("no language loaded").tts.clone()
    }

    fn languages(&self) -> Vec<String> {
        self.languages.keys().cloned().collect()
    }

    fn identifies(&self) -> bool {
        self.identifier.is_some()
    }

    fn identify(&self, samples_16k: &[f32]) -> Option<anyhow::Result<Scores>> {
        Some(self.identifier.as_ref()?.identify(samples_16k))
    }
}

pub const KOKORO: &str = "kokoro";

/// The Kokoro speaker id for `voice`, or the model's default for an unknown or empty one.
pub fn resolve_sid(kokoro: &KokoroModel, voice: &str) -> i32 {
    kokoro.speakers.iter().find(|v| v.id == voice).or(kokoro.speakers.first()).map_or(0, |v| v.sid)
}

fn path_str(p: &Path) -> anyhow::Result<String> {
    p.to_str().map(str::to_owned).ok_or_else(|| anyhow!("{} is not UTF-8", p.display()))
}

fn recognizer(stt: &SttModel) -> anyhow::Result<Recognizer> {
    match stt {
        SttModel::OnlineTransducer { encoder, decoder, joiner, tokens } => {
            let config = OnlineRecognizerConfig {
                model_config: OnlineModelConfig {
                    transducer: OnlineTransducerModelConfig {
                        encoder: Some(path_str(encoder)?),
                        decoder: Some(path_str(decoder)?),
                        joiner: Some(path_str(joiner)?),
                    },
                    tokens: Some(path_str(tokens)?),
                    num_threads: 4,
                    provider: Some("cpu".into()),
                    ..Default::default()
                },
                decoding_method: Some("greedy_search".into()),
                enable_endpoint: false,
                ..Default::default()
            };
            let recognizer =
                OnlineRecognizer::create(&config).ok_or_else(|| anyhow!("sherpa-onnx refused the recognizer config"))?;
            Ok(Recognizer::Online(Arc::new(recognizer)))
        }
        SttModel::OfflineTransducer { encoder, decoder, joiner, tokens } => {
            let config = OfflineRecognizerConfig {
                model_config: OfflineModelConfig {
                    transducer: OfflineTransducerModelConfig {
                        encoder: Some(path_str(encoder)?),
                        decoder: Some(path_str(decoder)?),
                        joiner: Some(path_str(joiner)?),
                    },
                    tokens: Some(path_str(tokens)?),
                    num_threads: 4,
                    provider: Some("cpu".into()),
                    model_type: Some("transducer".into()),
                    ..Default::default()
                },
                decoding_method: Some("greedy_search".into()),
                ..Default::default()
            };
            let recognizer =
                OfflineRecognizer::create(&config).ok_or_else(|| anyhow!("sherpa-onnx refused the recognizer config"))?;
            Ok(Recognizer::Offline(Arc::new(recognizer)))
        }
    }
}

impl Decoder for OfflineRecognizer {
    fn decode(&self, samples_16k: &[f32]) -> String {
        let stream = self.create_stream();
        stream.accept_waveform(SAMPLE_RATE, samples_16k);
        OfflineRecognizer::decode(self, &stream);
        stream.get_result().map(|r| r.text).unwrap_or_default()
    }
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

    fn reset(&mut self) {
        self.stream = self.recognizer.create_stream();
    }
}

struct OrtTurn {
    session: Mutex<ort::session::Session>,
    warned: AtomicBool,
}

/// Binds ort to the onnxruntime sherpa-onnx already loaded, so the process holds one;
/// `ORT_DYLIB_PATH` when none is loaded yet.
pub(crate) fn init_ort() -> anyhow::Result<()> {
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
    model: KokoroModel,
    language: String,
}

impl SherpaTts {
    fn create(language: &str, model: &KokoroModel, device: Device, gpu: GpuAttempt) -> anyhow::Result<Self> {
        let config = |provider: &str| -> anyhow::Result<OfflineTtsConfig> {
            Ok(OfflineTtsConfig {
                model: OfflineTtsModelConfig {
                    kokoro: OfflineTtsKokoroModelConfig {
                        model: Some(path_str(&model.model)?),
                        voices: Some(path_str(&model.voices)?),
                        tokens: Some(path_str(&model.tokens)?),
                        data_dir: Some(path_str(&model.data_dir)?),
                        lexicon: model.lexicon.clone(),
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
        Ok(SherpaTts { tts, provider, model: model.clone(), language: language.into() })
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
        let config = GenerationConfig { sid: resolve_sid(&self.model, voice), ..Default::default() };
        let audio = self
            .tts
            .generate_with_config::<fn(&[f32], f32) -> bool>(text, &config, None)
            .ok_or_else(|| anyhow!("synthesizing {text:?} failed"))?;
        Ok(audio.samples().to_vec())
    }
}

impl SherpaTts {
    fn voices(&self) -> Vec<VoiceInfo> {
        self.model
            .speakers
            .iter()
            .map(|v| VoiceInfo { id: v.id.clone(), label: v.label.clone(), languages: vec![self.language.clone()], credit: None })
            .collect()
    }
}

impl Renderer for SherpaTts {
    fn render(&self, text: &str, voice: &str) -> anyhow::Result<Vec<i16>> {
        Ok(to_48k_i16(&self.generate(text, voice)?))
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
    use crate::config::{models_from_dir, TtsConfig, VoiceEntry};

    fn models() -> Option<ModelsConfig> {
        std::env::var_os("NOTE_VOICE_MODELS").map(|d| models_from_dir(Path::new(&d), &TtsConfig::default()))
    }

    fn kokoro(e: &Engines, language: &str) -> Arc<dyn SpeechBackend> {
        let BaseVoice::Kokoro(kokoro) = e.tts(language) else { panic!("{language} speaks through a sidecar") };
        kokoro
    }

    fn test_model() -> KokoroModel {
        let mut model = models_from_dir(Path::new("/nonexistent"), &TtsConfig::default()).remove("en").unwrap().kokoro.unwrap();
        model.speakers = vec![
            VoiceEntry { id: "af_sarah".into(), sid: 3, label: "Sarah".into() },
            VoiceEntry { id: "bm_george".into(), sid: 9, label: "George".into() },
        ];
        model
    }

    #[test]
    #[ignore = "needs NOTE_VOICE_MODELS"]
    fn tts_then_stt_round_trips_a_sentence() {
        let e = Engines::load(&models().unwrap(), Device::Auto).unwrap();
        let pcm48 = kokoro(&e, "en").render("Move my run to tomorrow at seven.", "").unwrap();
        let pcm16: Vec<f32> = pcm48.iter().step_by(3).map(|s| f32::from(*s) / 32768.0).collect();
        let mut stt = e.stt("en").unwrap();
        for c in pcm16.chunks(2560) {
            stt.accept(c);
        }
        let text = stt.finish().to_lowercase();
        assert!(text.contains("run") && text.contains("tomorrow"), "{text}");
    }

    #[test]
    #[ignore = "needs NOTE_VOICE_MODELS with the Japanese models"]
    fn japanese_speech_is_recognized_offline() {
        let dir = PathBuf::from(std::env::var_os("NOTE_VOICE_MODELS").unwrap());
        let mut sets = models().unwrap();
        sets.remove("en");
        assert!(sets.contains_key("ja"), "no Japanese models under {}", dir.display());
        let e = Engines::load(&sets, Device::Cpu).unwrap();
        let wav = dir.join(crate::config::REAZON_DIR).join("test_wavs/2.wav");
        let mut reader = hound::WavReader::open(&wav).unwrap();
        assert_eq!(reader.spec().sample_rate, 16_000);
        let pcm16: Vec<f32> = reader.samples::<i16>().map(|s| f32::from(s.unwrap()) / 32768.0).collect();
        let mut stt = e.stt("ja").unwrap();
        let mut partials = Vec::new();
        for c in pcm16.chunks(2560) {
            stt.accept(c);
            let partial = stt.partial();
            if partials.last() != Some(&partial) {
                partials.push(partial);
            }
        }
        let start = std::time::Instant::now();
        let text = stt.finish();
        println!("ja: {text:?} in {:.2?}, partials {partials:?}", start.elapsed());
        assert!(text.contains("おじいさん"), "{text}");
        assert!(partials.len() > 1, "{partials:?}");
    }

    #[test]
    #[ignore = "needs NOTE_VOICE_MODELS"]
    fn a_failed_cuda_engine_falls_back_to_cpu() {
        let mut m = models().unwrap();
        let tts = SherpaTts::create("en", m["en"].kokoro.as_ref().unwrap(), Device::Cuda, GpuAttempt::Fail).unwrap();
        assert_eq!(tts.provider(), "cpu");
        assert!(!tts.render("Hi.", "").unwrap().is_empty());
        m.clear();
        assert!(Engines::load(&m, Device::Auto).unwrap().languages().is_empty());
    }

    #[test]
    #[ignore = "needs NOTE_VOICE_MODELS"]
    fn smart_turn_scores_a_finished_sentence_above_a_cut_one() {
        let e = Engines::load(&models().unwrap(), Device::Cpu).unwrap();
        let tts = kokoro(&e, "en");
        let down = |p: Vec<i16>| p.iter().step_by(3).map(|s| f32::from(*s) / 32768.0).collect::<Vec<f32>>();
        let done = down(tts.render("Can you move my run to tomorrow?", "").unwrap());
        let cut = down(tts.render("Can you move my", "").unwrap());
        let t = e.turn("en");
        assert!(t.complete(&done) > t.complete(&cut));
    }

    const REPLY: [&str; 4] = [
        "Sure, I moved your run to tomorrow at seven.",
        "The weather looks clear,",
        "so it should be a good morning for it.",
        "Do you want a reminder the night before?",
    ];

    #[test]
    #[ignore = "needs NOTE_VOICE_MODELS"]
    fn kokoro_speaks_a_three_sentence_reply_in_two_or_three_chunks() {
        use super::super::tts::Next;
        let e = Engines::load(&models().unwrap(), Device::Auto).unwrap();
        let mut stream = kokoro(&e, "en").open("").unwrap();
        for clause in REPLY {
            stream.push(clause).unwrap();
        }
        stream.finish().unwrap();
        let mut pieces = Vec::new();
        while let Next::Audio(audio) = stream.next(std::time::Duration::MAX).unwrap() {
            assert!(!audio.pcm.is_empty());
            pieces.push(audio.chars);
        }
        assert!((2..=3).contains(&pieces.len()), "{pieces:?}");
        assert_eq!(pieces.iter().sum::<u32>(), REPLY.iter().map(|c| c.len() as u32).sum::<u32>());
    }

    /// Plays `backend` speaking `REPLY` in real time, its clauses `apart`; returns the time to the first
    /// audio, the silences after it, and the pieces rendered.
    fn play_in_real_time(
        backend: Arc<dyn super::super::tts::SpeechBackend>,
        apart: std::time::Duration,
    ) -> (std::time::Duration, Vec<std::time::Duration>, usize) {
        use super::super::playout::Playout;
        use super::super::speech::SpeechQueue;
        use super::super::tts::Speaker;
        use std::time::{Duration, Instant};
        const TICK: Duration = Duration::from_millis(10);
        let mut q = SpeechQueue::new(Speaker::new(backend.clone(), ""), backend);
        let mut p = Playout::default();
        q.play(1);
        let start = Instant::now();
        let (mut said, mut first, mut silent, mut gaps, mut pieces) = (0, None, 0u32, Vec::new(), 0);
        for tick in 0u32.. {
            std::thread::sleep((start + TICK * tick).saturating_duration_since(Instant::now()));
            while said < REPLY.len() && start.elapsed() >= apart * said as u32 {
                q.speak(1, said as u32, REPLY[said].into());
                said += 1;
                if said == REPLY.len() {
                    q.speak_done(1);
                }
            }
            let queued = p.queued_samples();
            q.pump(&mut p);
            if p.queued_samples() > queued {
                pieces += 1;
            }
            if p.next_frame().is_some() {
                first.get_or_insert(start.elapsed());
                if silent > 0 {
                    gaps.push(TICK * silent);
                    silent = 0;
                }
            } else if first.is_some() {
                silent += 1;
            }
            if q.take_finished(&mut p) == [1] {
                break;
            }
        }
        (first.unwrap(), gaps, pieces)
    }

    /// Speaks each clause on its own, as one piece.
    struct PerClause(Arc<dyn super::super::tts::SpeechBackend>);

    struct PerClauseStream(Arc<dyn super::super::tts::SpeechBackend>, std::collections::VecDeque<String>, bool);

    impl super::super::tts::SpeechBackend for PerClause {
        fn id(&self) -> &'static str {
            "per-clause"
        }

        fn label(&self) -> &'static str {
            "Per clause"
        }

        fn input(&self) -> super::super::tts::TextInput {
            super::super::tts::TextInput::Incremental
        }

        fn voices(&self) -> Vec<VoiceInfo> {
            Vec::new()
        }

        fn open(&self, _voice: &str) -> anyhow::Result<Box<dyn super::super::tts::SpeechStream>> {
            Ok(Box::new(PerClauseStream(self.0.clone(), std::collections::VecDeque::new(), false)))
        }
    }

    impl super::super::tts::SpeechStream for PerClauseStream {
        fn push(&mut self, text: &str) -> anyhow::Result<()> {
            self.1.push_back(text.into());
            Ok(())
        }

        fn finish(&mut self) -> anyhow::Result<()> {
            self.2 = true;
            Ok(())
        }

        fn next(&mut self, _ahead: std::time::Duration) -> anyhow::Result<super::super::tts::Next> {
            use super::super::tts::{Audio, Next};
            Ok(match self.1.pop_front() {
                Some(text) => Next::Audio(Audio { pcm: self.0.render(&text, "")?, chars: text.chars().count() as u32 }),
                None if self.2 => Next::Done,
                None => Next::Pending,
            })
        }

        fn cancel(&mut self) {}
    }

    #[test]
    #[ignore = "needs NOTE_VOICE_MODELS; prints timings"]
    fn streaming_against_per_clause_timing() {
        use std::time::Duration;
        for device in [Device::Auto, Device::Cpu] {
            let e = Engines::load(&models().unwrap(), device).unwrap();
            let kokoro = kokoro(&e, "en");
            for apart in [Duration::ZERO, Duration::from_millis(150), Duration::from_millis(400)] {
                for (name, backend) in [
                    ("per clause", Arc::new(PerClause(kokoro.clone())) as Arc<dyn super::super::tts::SpeechBackend>),
                    ("chunked", kokoro.clone()),
                ] {
                    let (first, gaps, pieces) = play_in_real_time(backend, apart);
                    println!(
                        "{device:?}, clauses {apart:?} apart, {name}: first audio at {first:?}, {pieces} pieces, gaps {gaps:?} (total {:?})",
                        gaps.iter().sum::<Duration>()
                    );
                }
            }
        }
    }

    #[test]
    fn kokoro_audio_doubles_to_48k_with_midpoints_and_clamps() {
        assert_eq!(to_48k_i16(&[0.0, 0.5, 2.0]), vec![0, 8192, 16384, 32767, 32767, 32767]);
    }

    #[test]
    fn an_unknown_voice_falls_back_to_the_language_default() {
        let model = test_model();
        assert_eq!(resolve_sid(&model, "bm_george"), 9);
        assert_eq!(resolve_sid(&model, "nope"), model.speakers[0].sid);
        assert_eq!(resolve_sid(&model, ""), model.speakers[0].sid);
    }

    #[test]
    fn a_sidecar_base_voice_is_live_only_while_its_sidecar_answers() {
        use super::super::tts::ChunkedBackend;
        struct Silent;
        impl Renderer for Silent {
            fn render(&self, _text: &str, _voice: &str) -> anyhow::Result<Vec<i16>> {
                Ok(Vec::new())
            }
        }
        let ja: Arc<dyn SpeechBackend> = Arc::new(ChunkedBackend::new("ja", "Japanese", Arc::new(Silent), Vec::new()));
        let base = BaseVoice::Sidecar("ja".into());
        assert_eq!(base.id(), "ja");
        assert!(base.live(&[]).is_none());
        assert_eq!(base.live(std::slice::from_ref(&ja)).map(|b| b.id().to_owned()).as_deref(), Some("ja"));
        let kokoro: Arc<dyn SpeechBackend> = Arc::new(ChunkedBackend::new(KOKORO, "Kokoro", Arc::new(Silent), Vec::new()));
        assert!(BaseVoice::Kokoro(kokoro).live(&[]).is_some());
    }
}
