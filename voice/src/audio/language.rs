use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Context};

use super::mel::{whisper_log_mel, MEL_BINS};

/// The least probability, over every language Whisper knows, that the top language needs to be taken.
pub const CONFIDENT: f32 = 0.5;
/// The same for under `SHORT` seconds of speech, which Whisper-tiny misjudges more often.
pub const CONFIDENT_SHORT: f32 = 0.8;
pub const SHORT: f32 = 0.8;
/// Frames of the clip's floor after the speech; Whisper-tiny reads a clip cut off at the last word
/// less surely than one that trails into quiet.
const PAD_FRAMES: usize = 300;
/// Below this the features cannot be framed.
const MIN_SAMPLES: usize = 16_000 / 4;

/// Language codes with their probabilities, most likely first.
pub type Scores = Vec<(String, f32)>;

pub trait LanguageId: Send + Sync {
    fn identify(&self, samples_16k: &[f32]) -> anyhow::Result<Scores>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub language: String,
    /// False when the scores did not decide it and `fallback` stood in.
    pub identified: bool,
}

/// The top language of `seconds` of speech when it is one the call can speak and confident enough;
/// else `fallback`.
pub fn choose(scores: Option<&[(String, f32)]>, seconds: f32, speakable: &[String], fallback: &str) -> Choice {
    let confident = if seconds < SHORT { CONFIDENT_SHORT } else { CONFIDENT };
    match scores.and_then(|s| s.first()) {
        Some((top, p)) if *p >= confident && speakable.contains(top) => Choice { language: top.clone(), identified: true },
        _ => Choice { language: fallback.to_owned(), identified: false },
    }
}

/// Spoken language identification on the sherpa-onnx export of Whisper-tiny: one encoder pass and one
/// decoder step from start-of-transcript, read as a distribution over the language tokens.
pub struct WhisperLanguageId {
    encoder: Mutex<ort::session::Session>,
    decoder: Mutex<ort::session::Session>,
    sot: i64,
    /// Each language's code and its token.
    languages: Vec<(String, usize)>,
    cache: [usize; 4],
}

impl WhisperLanguageId {
    pub fn create(encoder: &Path, decoder: &Path) -> anyhow::Result<Self> {
        super::engines::init_ort()?;
        let session = |path: &Path| -> anyhow::Result<ort::session::Session> {
            ort::session::Session::builder()?
                .with_intra_threads(4)
                .map_err(|e| anyhow!("{e}"))?
                .commit_from_file(path)
                .with_context(|| format!("loading {}", path.display()))
        };
        let encoder = session(encoder)?;
        let decoder = session(decoder)?;
        let meta = encoder.metadata()?;
        let field = |key: &str| meta.custom(key).ok_or_else(|| anyhow!("the encoder has no {key:?} metadata"));
        let number = |key: &str| -> anyhow::Result<usize> { Ok(field(key)?.trim().parse()?) };
        let tokens: Vec<usize> = field("all_language_tokens")?.split(',').map(|t| t.trim().parse()).collect::<Result<_, _>>()?;
        let codes: Vec<String> = field("all_language_codes")?.split(',').map(|c| c.trim().to_owned()).collect();
        anyhow::ensure!(tokens.len() == codes.len() && !tokens.is_empty(), "the language tokens and codes do not pair up");
        let cache = [number("n_text_layer")?, 1, number("n_text_ctx")?, number("n_text_state")?];
        let sot = i64::try_from(number("sot")?)?;
        drop(meta);
        let id = WhisperLanguageId {
            encoder: Mutex::new(encoder),
            decoder: Mutex::new(decoder),
            sot,
            languages: codes.into_iter().zip(tokens).collect(),
            cache,
        };
        id.identify(&vec![0.0; MIN_SAMPLES]).context("warming the language identifier")?;
        Ok(id)
    }
}

impl LanguageId for WhisperLanguageId {
    fn identify(&self, samples_16k: &[f32]) -> anyhow::Result<Scores> {
        anyhow::ensure!(samples_16k.len() >= MIN_SAMPLES, "{} samples are too few to identify", samples_16k.len());
        let (mel, frames) = whisper_log_mel(samples_16k, PAD_FRAMES);
        let mel = ort::value::Tensor::from_array(([1usize, MEL_BINS, frames], mel))?;
        let (cross_k, cross_v) = {
            let mut encoder = self.encoder.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut out = encoder.run(ort::inputs![mel])?;
            let k = out.remove("n_layer_cross_k").ok_or_else(|| anyhow!("the encoder gave no cross keys"))?;
            let v = out.remove("n_layer_cross_v").ok_or_else(|| anyhow!("the encoder gave no cross values"))?;
            (k, v)
        };
        let zeros = || ort::value::Tensor::from_array((self.cache, vec![0.0f32; self.cache.iter().product()]));
        let tokens = ort::value::Tensor::from_array(([1usize, 1], vec![self.sot]))?;
        let offset = ort::value::Tensor::from_array(([1usize], vec![0i64]))?;
        let mut decoder = self.decoder.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let out = decoder.run(ort::inputs![tokens, zeros()?, zeros()?, cross_k, cross_v, offset])?;
        let (_, logits) = out[0].try_extract_tensor::<f32>()?;
        let logit = |token: usize| logits.get(token).copied().ok_or_else(|| anyhow!("no logit for token {token}"));
        let raw: Vec<f32> = self.languages.iter().map(|(_, t)| logit(*t)).collect::<anyhow::Result<_>>()?;
        let peak = raw.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let exp: Vec<f32> = raw.iter().map(|l| (l - peak).exp()).collect();
        let total: f32 = exp.iter().sum();
        let mut scores: Scores = self.languages.iter().zip(exp).map(|((code, _), e)| (code.clone(), e / total)).collect();
        scores.sort_by(|a, b| b.1.total_cmp(&a.1));
        Ok(scores)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scores(top: &str, p: f32) -> Scores {
        vec![(top.into(), p), ("de".into(), (1.0 - p) / 2.0)]
    }

    fn both() -> Vec<String> {
        vec!["en".into(), "ja".into()]
    }

    #[test]
    fn a_confident_supported_language_is_taken() {
        assert_eq!(choose(Some(&scores("ja", 0.9)), 2.0, &both(), "en"), Choice { language: "ja".into(), identified: true });
        assert_eq!(choose(Some(&scores("en", 0.8)), 2.0, &both(), "ja"), Choice { language: "en".into(), identified: true });
    }

    #[test]
    fn an_unsupported_top_language_falls_back_to_the_setting() {
        let choice = choose(Some(&scores("ko", 0.95)), 2.0, &both(), "ja");
        assert_eq!(choice, Choice { language: "ja".into(), identified: false });
        let choice = choose(Some(&scores("ja", 0.95)), 2.0, &["en".to_string()], "en");
        assert_eq!(choice, Choice { language: "en".into(), identified: false }, "a language without models is unsupported");
    }

    #[test]
    fn a_low_confidence_or_missing_result_falls_back() {
        assert_eq!(choose(Some(&scores("ja", 0.45)), 2.0, &both(), "en"), Choice { language: "en".into(), identified: false });
        assert_eq!(choose(None, 2.0, &both(), "ja"), Choice { language: "ja".into(), identified: false });
        assert_eq!(choose(Some(&[]), 2.0, &both(), "en").language, "en");
        assert_eq!(choose(Some(&scores("en", 0.6)), 0.5, &both(), "ja").language, "ja", "a word or two must be surer");
        assert!(choose(Some(&scores("en", 0.9)), 0.5, &both(), "ja").identified);
    }

    fn model() -> Option<WhisperLanguageId> {
        let dir = std::path::PathBuf::from(std::env::var_os("NOTE_VOICE_MODELS")?).join(crate::config::WHISPER_DIR);
        Some(WhisperLanguageId::create(&dir.join("tiny-encoder.int8.onnx"), &dir.join("tiny-decoder.int8.onnx")).unwrap())
    }

    fn wav(path: &Path) -> Vec<f32> {
        let mut reader = hound::WavReader::open(path).unwrap();
        assert_eq!(reader.spec().sample_rate, 16_000, "{}", path.display());
        reader.samples::<i16>().map(|s| f32::from(s.unwrap()) / 32768.0).collect()
    }

    /// Runs on the first 2.5 s of each clip in `NOTE_VOICE_LID_CLIPS`, a directory of 16 kHz WAVs each named for its language
    /// (`en-….wav`, `ja-….wav`), else the Japanese test clips shipped with the models.
    #[test]
    #[ignore = "needs NOTE_VOICE_MODELS; prints accuracy and timings"]
    fn whisper_tiny_names_the_language_of_short_clips() {
        let id = model().expect("NOTE_VOICE_MODELS is not set");
        let models = std::path::PathBuf::from(std::env::var_os("NOTE_VOICE_MODELS").unwrap());
        let dir = std::env::var_os("NOTE_VOICE_LID_CLIPS").map(std::path::PathBuf::from);
        let mut clips: Vec<(String, std::path::PathBuf)> = match &dir {
            Some(dir) => std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.extension().is_some_and(|e| e == "wav"))
                .map(|p| (p.file_name().unwrap().to_string_lossy()[..2].to_owned(), p))
                .collect(),
            None => vec![("ja".into(), models.join(crate::config::REAZON_DIR).join("test_wavs/2.wav"))],
        };
        clips.sort();
        let (mut right, mut taken, mut wrong, mut times) = (0, 0, 0, Vec::new());
        for (want, path) in &clips {
            let samples = wav(path);
            let samples = &samples[..samples.len().min(16_000 * 5 / 2)];
            let start = std::time::Instant::now();
            let scores = id.identify(samples).unwrap();
            times.push(start.elapsed());
            let choice = choose(Some(&scores), samples.len() as f32 / 16_000.0, &["en".to_string(), "ja".to_string()], "--");
            right += usize::from(scores[0].0 == *want);
            taken += usize::from(choice.identified && choice.language == *want);
            wrong += usize::from(choice.identified && choice.language != *want);
            println!(
                "{:<24} {:<3} {:.2}  then {:<3} {:.2}  {:?}",
                path.file_name().unwrap().to_string_lossy(),
                scores[0].0,
                scores[0].1,
                scores[1].0,
                scores[1].1,
                times.last().unwrap()
            );
        }
        times.sort();
        println!(
            "{right}/{} top-1 right; {taken} taken, {wrong} taken wrongly, the rest fall back; median {:?}, worst {:?}",
            clips.len(),
            times[times.len() / 2],
            times.last().unwrap()
        );
        assert_eq!(wrong, 0, "a clip was taken as a language it is not in");
    }
}
