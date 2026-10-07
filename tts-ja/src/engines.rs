use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use crate::sbv2::{self, Sbv2};
use crate::voicevox::Voicevox;
use crate::{audio, text};

pub const SAMPLE_RATE: u32 = 48000;
const HIMARI_STYLE: u32 = 14;
const WARM_UP: [&str; 2] = ["テストです。", "今日はいい天気ですね、散歩に行きましょう。"];
/// Slower than this on the CPU, アミ is left to VOICEVOX.
const MAX_CPU_RTF: f64 = 0.5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Voice {
    Ami,
    Himari,
}

enum Engine {
    Sbv2,
    Voicevox,
}

pub struct VoiceMeta {
    pub voice: Voice,
    pub id: &'static str,
    pub label: &'static str,
    pub credit: &'static str,
}

pub const VOICES: [VoiceMeta; 2] = [
    VoiceMeta {
        voice: Voice::Ami,
        id: "ami",
        label: "小春音アミ",
        credit: "Style-Bert-VITS2モデル: 小春音アミ、あみたろの声素材工房 (https://amitaro.net/)",
    },
    VoiceMeta {
        voice: Voice::Himari,
        id: "himari",
        label: "冥鳴ひまり（VOICEVOX）",
        credit: "VOICEVOX:冥鳴ひまり",
    },
];

pub struct Settings {
    pub sbv2_dir: Option<PathBuf>,
    pub voicevox_dir: PathBuf,
    pub gpu: bool,
}

#[derive(Default)]
struct Loaded {
    sbv2: Option<Sbv2>,
    voicevox: Option<Voicevox>,
}

/// Both engines behind one lock; each voice is spoken by the other engine when its own is
/// missing or fails, so a stream never goes silent.
#[derive(Default)]
pub struct Engines {
    loaded: Mutex<Loaded>,
    ready: AtomicBool,
}

impl Engines {
    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// VOICEVOX first, as it loads in about a second, then Style-Bert-VITS2; each warmed up.
    pub fn load(&self, settings: &Settings) -> anyhow::Result<()> {
        let t0 = Instant::now();
        let threads = num_cpus::get_physical().min(8) as u16;
        match Voicevox::load(&settings.voicevox_dir, HIMARI_STYLE, threads).and_then(|mut v| {
            v.render(WARM_UP[0])?;
            Ok(v)
        }) {
            Ok(v) => {
                self.lock().voicevox = Some(v);
                self.ready.store(true, Ordering::Release);
                eprintln!(
                    "tts-ja: VOICEVOX ready in {:.1} s",
                    t0.elapsed().as_secs_f64()
                );
            }
            Err(e) => eprintln!("tts-ja: VOICEVOX failed to load: {e:#}"),
        }

        if settings.sbv2_dir.is_none() {
            eprintln!("tts-ja: Style-Bert-VITS2 is off, VOICEVOX speaks for アミ");
        }
        if let Some(dir) = &settings.sbv2_dir {
            let t0 = Instant::now();
            match load_sbv2(dir, settings.gpu) {
                Ok(s) => {
                    eprintln!(
                        "tts-ja: Style-Bert-VITS2 ready on the {} in {:.1} s",
                        if s.on_gpu { "GPU" } else { "CPU" },
                        t0.elapsed().as_secs_f64()
                    );
                    self.lock().sbv2 = Some(s);
                    self.ready.store(true, Ordering::Release);
                }
                Err(e) => eprintln!(
                    "tts-ja: Style-Bert-VITS2 is unavailable, VOICEVOX speaks for アミ: {e:#}"
                ),
            }
        }
        anyhow::ensure!(self.ready(), "neither engine loaded");
        Ok(())
    }

    /// s16 PCM at `SAMPLE_RATE`: the voice's own engine, else the other one. Empty when `raw`
    /// holds nothing to say or neither engine can say it.
    pub fn render(&self, voice: Voice, raw: &str) -> anyhow::Result<Vec<i16>> {
        let text = text::prepare(raw);
        if !text::speakable(&text) {
            return Ok(Vec::new());
        }
        let mut loaded = self.lock();
        let Loaded { sbv2, voicevox } = &mut *loaded;
        anyhow::ensure!(sbv2.is_some() || voicevox.is_some(), "no engine is loaded");
        let order = match voice {
            Voice::Ami => [Engine::Sbv2, Engine::Voicevox],
            Voice::Himari => [Engine::Voicevox, Engine::Sbv2],
        };
        for engine in order {
            let (name, rendered) = match engine {
                Engine::Sbv2 => (
                    "Style-Bert-VITS2",
                    sbv2.as_mut().map(|s| {
                        catch_unwind(AssertUnwindSafe(|| {
                            s.render(&text).map(|a| finish(&a, sbv2::SAMPLE_RATE))
                        }))
                    }),
                ),
                Engine::Voicevox => (
                    "VOICEVOX",
                    voicevox.as_mut().map(|v| {
                        catch_unwind(AssertUnwindSafe(|| {
                            v.render(&text).map(|(a, rate)| finish(&a, rate))
                        }))
                    }),
                ),
            };
            match rendered {
                None => {}
                Some(Ok(Ok(pcm))) => return Ok(pcm),
                Some(Ok(Err(e))) => eprintln!("tts-ja: {name} failed on {text:?}: {e:#}"),
                Some(Err(_)) => eprintln!("tts-ja: {name} panicked on {text:?}"),
            }
        }
        eprintln!("tts-ja: skipping {text:?}, which neither engine could say");
        Ok(Vec::new())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Loaded> {
        self.loaded.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// On the GPU when asked and possible, else on the CPU only if fast enough to keep up.
fn load_sbv2(dir: &std::path::Path, gpu: bool) -> anyhow::Result<Sbv2> {
    let mut sbv2 = match Sbv2::load(dir, gpu) {
        Ok(s) => s,
        Err(e) if gpu => {
            eprintln!("tts-ja: Style-Bert-VITS2 cannot use the GPU, trying the CPU: {e:#}");
            Sbv2::load(dir, false)?
        }
        Err(e) => return Err(e),
    };
    for line in WARM_UP {
        sbv2.render(line)?;
    }
    if !sbv2.on_gpu {
        let t0 = Instant::now();
        let samples = sbv2.render(WARM_UP[1])?;
        let rtf =
            t0.elapsed().as_secs_f64() / (samples.len() as f64 / f64::from(sbv2::SAMPLE_RATE));
        anyhow::ensure!(
            rtf <= MAX_CPU_RTF,
            "too slow on the CPU (real-time factor {rtf:.2})"
        );
    }
    Ok(sbv2)
}

fn finish(samples: &[f32], rate: u32) -> Vec<i16> {
    audio::to_pcm(&audio::resample(samples, rate, SAMPLE_RATE))
}
