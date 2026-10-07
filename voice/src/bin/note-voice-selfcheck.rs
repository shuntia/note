use std::path::{Path, PathBuf};
use std::time::Instant;

use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::AudioSourceOptions;
use note_voice::audio::engines::{Device, Engines, SpeechEngines, SpeechToText};
use note_voice::config::{models_from_dir, TtsConfig, REAZON_DIR};

const SENTENCE: &str = "Move my run to tomorrow at seven.";
/// 160 ms of 16 kHz audio, as a call feeds the recognizer.
const CHUNK: usize = 2560;

fn main() -> anyhow::Result<()> {
    let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 100);
    println!("livekit {} (webrtc audio source at {} Hz)", livekit::SDK_VERSION, source.sample_rate());
    println!(
        "sherpa-onnx {} ({}), onnxruntime {}",
        sherpa_onnx::version(),
        sherpa_onnx::git_sha1(),
        sherpa_onnx::onnxruntime_version()
    );

    let dir = PathBuf::from(std::env::var_os("NOTE_VOICE_MODELS").ok_or_else(|| anyhow::anyhow!("NOTE_VOICE_MODELS is not set"))?);
    let device = match std::env::var("NOTE_VOICE_DEVICE").as_deref() {
        Ok("cpu") => Device::Cpu,
        Ok("cuda") => Device::Cuda,
        _ => Device::Auto,
    };
    let t = Instant::now();
    let engines = Engines::load(&models_from_dir(&dir, &TtsConfig::default()), device)?;
    let languages = engines.languages();
    println!("engines loaded in {:.2?} (languages: {languages:?})", t.elapsed());
    println!("onnxruntime libraries mapped: {}", onnxruntime_images()?);

    if languages.iter().any(|l| l == "en") {
        english(&engines)?;
    }
    if languages.iter().any(|l| l == "ja") {
        let wav = std::env::var_os("NOTE_VOICE_JA_WAV").map_or_else(|| dir.join(REAZON_DIR).join("test_wavs/2.wav"), PathBuf::from);
        japanese(&engines, &wav)?;
    }
    Ok(())
}

fn english(engines: &Engines) -> anyhow::Result<()> {
    let tts = engines.tts("en").live(&[]).ok_or_else(|| anyhow::anyhow!("English has no in-process voice"))?;
    let t = Instant::now();
    let pcm48 = tts.render(SENTENCE, "")?;
    println!("en tts: {:.2} s of audio in {:.2?}", pcm48.len() as f64 / 48_000.0, t.elapsed());

    let pcm16: Vec<f32> = pcm48.iter().step_by(3).map(|s| f32::from(*s) / 32768.0).collect();
    let (text, took, _) = transcribe(engines.stt("en")?, &pcm16);
    println!("en stt: {text:?} in {took:.2?}");

    let t = Instant::now();
    let p = engines.turn("en").complete(&pcm16);
    println!("en turn: {p:.3} complete in {:.2?}", t.elapsed());
    Ok(())
}

/// Decodes a 16 kHz mono WAV of Japanese speech, printing the transcript, the partials and the timings.
fn japanese(engines: &Engines, wav: &Path) -> anyhow::Result<()> {
    let mut reader = hound::WavReader::open(wav).map_err(|e| anyhow::anyhow!("reading {}: {e}", wav.display()))?;
    let spec = reader.spec();
    anyhow::ensure!(spec.sample_rate == 16_000 && spec.channels == 1, "{} is not 16 kHz mono", wav.display());
    let pcm16: Vec<f32> = reader.samples::<i16>().map(|s| s.map(|s| f32::from(s) / 32768.0)).collect::<Result<_, _>>()?;
    println!("ja clip: {} ({:.2} s)", wav.display(), pcm16.len() as f64 / 16_000.0);
    let (text, took, partials) = transcribe(engines.stt("ja")?, &pcm16);
    println!("ja stt: {text:?}; the finish took {took:.2?}");
    println!("ja partials: {} updates, the slowest {:.2?}", partials.len(), partials.iter().map(|(_, d)| *d).max().unwrap_or_default());
    for (partial, took) in &partials {
        println!("  {took:>8.2?}  {partial}");
    }
    let t = Instant::now();
    let p = engines.turn("ja").complete(&pcm16);
    println!("ja turn: {p:.3} complete in {:.2?}", t.elapsed());
    Ok(())
}

/// Feeds the audio in real time, as a call does; returns the final text, how long the finish took,
/// and each distinct partial with how long its step took.
fn transcribe(mut stt: Box<dyn SpeechToText>, pcm16: &[f32]) -> (String, std::time::Duration, Vec<(String, std::time::Duration)>) {
    let mut partials: Vec<(String, std::time::Duration)> = Vec::new();
    let start = Instant::now();
    for (i, chunk) in pcm16.chunks(CHUNK).enumerate() {
        let due = start + std::time::Duration::from_millis(160 * i as u64);
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
        let t = Instant::now();
        stt.accept(chunk);
        let partial = stt.partial();
        let took = t.elapsed();
        if partials.last().is_none_or(|(p, _)| *p != partial) && !partial.is_empty() {
            partials.push((partial, took));
        }
    }
    let t = Instant::now();
    let text = stt.finish();
    (text, t.elapsed(), partials)
}

fn onnxruntime_images() -> anyhow::Result<usize> {
    let maps = std::fs::read_to_string("/proc/self/maps")?;
    let mut libs: Vec<&str> =
        maps.lines().filter_map(|l| l.split_whitespace().nth(5)).filter(|p| p.contains("libonnxruntime.so")).collect();
    libs.sort_unstable();
    libs.dedup();
    Ok(libs.len())
}
