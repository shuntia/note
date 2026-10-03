use std::path::Path;
use std::time::Instant;

use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::AudioSourceOptions;
use note_voice::audio::engines::{Device, Engines, SpeechEngines};
use note_voice::config::models_from_dir;

const SENTENCE: &str = "Move my run to tomorrow at seven.";

fn main() -> anyhow::Result<()> {
    let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 100);
    println!("livekit {} (webrtc audio source at {} Hz)", livekit::SDK_VERSION, source.sample_rate());
    println!(
        "sherpa-onnx {} ({}), onnxruntime {}",
        sherpa_onnx::version(),
        sherpa_onnx::git_sha1(),
        sherpa_onnx::onnxruntime_version()
    );

    let dir = std::env::var_os("NOTE_VOICE_MODELS").ok_or_else(|| anyhow::anyhow!("NOTE_VOICE_MODELS is not set"))?;
    let t = Instant::now();
    let engines = Engines::load(&models_from_dir(Path::new(&dir)), Device::Auto)?;
    println!("engines loaded in {:.2?} (languages: {:?})", t.elapsed(), engines.languages());
    println!("onnxruntime libraries mapped: {}", onnxruntime_images()?);

    let tts = engines.tts("en");
    let t = Instant::now();
    let pcm48 = tts.synthesize(SENTENCE, "")?;
    println!("tts: {:.2} s of audio in {:.2?}", pcm48.len() as f64 / 48_000.0, t.elapsed());

    let pcm16: Vec<f32> = pcm48.iter().step_by(3).map(|s| f32::from(*s) / 32768.0).collect();
    let mut stt = engines.stt("en")?;
    let t = Instant::now();
    for chunk in pcm16.chunks(2560) {
        stt.accept(chunk);
    }
    let text = stt.finish();
    println!("stt: {text:?} in {:.2?}", t.elapsed());

    let t = Instant::now();
    let p = engines.turn("en").complete(&pcm16);
    println!("turn: {p:.3} complete in {:.2?}", t.elapsed());
    Ok(())
}

fn onnxruntime_images() -> anyhow::Result<usize> {
    let maps = std::fs::read_to_string("/proc/self/maps")?;
    let mut libs: Vec<&str> =
        maps.lines().filter_map(|l| l.split_whitespace().nth(5)).filter(|p| p.contains("libonnxruntime.so")).collect();
    libs.sort_unstable();
    libs.dedup();
    Ok(libs.len())
}
