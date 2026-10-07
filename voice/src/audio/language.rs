use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Context};

use super::mel::{whisper_log_mel, MEL_BINS};

/// Less speech than this is not judged at all; Whisper-tiny takes a word or two for the wrong language
/// with near certainty.
pub const MIN_SECONDS: f32 = 0.8;
/// The share of the speakable languages' probability the top one needs.
pub const CONFIDENT: f32 = 0.8;
/// The same for under `SHORT` seconds of speech.
pub const CONFIDENT_SHORT: f32 = 0.9;
pub const SHORT: f32 = 1.2;
/// Below this share of the whole distribution, Whisper heard none of the speakable languages.
pub const SPEAKABLE_FLOOR: f32 = 0.05;
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
    /// False when the scores never decided it and the user's language stood in.
    pub identified: bool,
}

/// The language of `seconds` of speech, judged among the `speakable` ones alone: the likeliest of
/// them once its share of their probability is confident, and they hold more than a trace of it.
pub fn confident(scores: &[(String, f32)], seconds: f32, speakable: &[String]) -> Option<String> {
    if seconds < MIN_SECONDS {
        return None;
    }
    let ours: Vec<&(String, f32)> = scores.iter().filter(|(l, _)| speakable.contains(l)).collect();
    let mass: f32 = ours.iter().map(|(_, p)| p).sum();
    let (top, p) = ours.into_iter().max_by(|a, b| a.1.total_cmp(&b.1))?;
    let needed = if seconds < SHORT { CONFIDENT_SHORT } else { CONFIDENT };
    (mass >= SPEAKABLE_FLOOR && p / mass >= needed).then(|| top.clone())
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

    fn scores(pairs: &[(&str, f32)]) -> Scores {
        pairs.iter().map(|(l, p)| ((*l).to_owned(), *p)).collect()
    }

    fn both() -> Vec<String> {
        vec!["en".into(), "ja".into()]
    }

    #[test]
    fn a_confident_speakable_language_is_taken() {
        assert_eq!(confident(&scores(&[("ja", 0.9), ("en", 0.05)]), 2.0, &both()).as_deref(), Some("ja"));
        assert_eq!(confident(&scores(&[("en", 0.8), ("de", 0.1), ("ja", 0.1)]), 2.0, &both()).as_deref(), Some("en"));
    }

    #[test]
    fn japanese_heard_as_chinese_is_still_japanese_among_the_speakable() {
        let heard = scores(&[("zh", 0.85), ("ja", 0.09), ("en", 0.014)]);
        assert_eq!(confident(&heard, 2.5, &both()).as_deref(), Some("ja"));
        let heard = scores(&[("ko", 0.77), ("ja", 0.21), ("en", 0.003)]);
        assert_eq!(confident(&heard, 1.7, &both()).as_deref(), Some("ja"));
    }

    #[test]
    fn a_split_short_or_foreign_result_is_not_confident() {
        assert_eq!(confident(&scores(&[("ja", 0.42), ("en", 0.14)]), 2.0, &both()), None, "a 75/25 split");
        assert_eq!(confident(&scores(&[("ko", 0.97), ("ja", 0.02), ("en", 0.001)]), 2.0, &both()), None, "barely any of ours");
        assert_eq!(confident(&scores(&[("ja", 0.99)]), 0.5, &both()), None, "a word or two is not judged");
        assert_eq!(confident(&scores(&[("ja", 0.85), ("en", 0.15)]), 1.0, &both()), None, "a short clip must be surer");
        assert_eq!(confident(&scores(&[("ja", 0.85), ("en", 0.15)]), 1.5, &both()).as_deref(), Some("ja"));
        assert_eq!(confident(&scores(&[("ja", 0.95)]), 2.0, &["en".to_string()]), None, "a language without models");
        assert_eq!(confident(&[], 2.0, &both()), None);
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
            let taken_as = confident(&scores, samples.len() as f32 / 16_000.0, &["en".to_string(), "ja".to_string()]);
            right += usize::from(scores[0].0 == *want);
            taken += usize::from(taken_as.as_deref() == Some(want.as_str()));
            wrong += usize::from(taken_as.as_ref().is_some_and(|l| l != want));
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
            "{right}/{} top-1 over all languages; {taken} taken, {wrong} taken wrongly, the rest left open; median {:?}, worst {:?}",
            clips.len(),
            times[times.len() / 2],
            times.last().unwrap()
        );
        assert_eq!(wrong, 0, "a clip was taken as a language it is not in");
    }
}
