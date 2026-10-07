use std::f64::consts::PI;
use std::sync::OnceLock;

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

pub const MEL_BINS: usize = 80;
pub const MEL_FRAMES: usize = 800;

const SAMPLE_RATE: usize = 16000;
const WINDOW_SAMPLES: usize = 8 * SAMPLE_RATE;
const N_FFT: usize = 400;
const HOP: usize = 160;
const FREQ_BINS: usize = N_FFT / 2 + 1;

/// Smart Turn v3 input features, as pipecat computes them: the last 8 s of 16 kHz audio,
/// zero-padded at the start, through Whisper's log-mel extractor with `do_normalize`.
/// Returns `MEL_BINS × MEL_FRAMES`, mel-major.
pub fn log_mel(samples_16k: &[f32]) -> Vec<f32> {
    let tail = &samples_16k[samples_16k.len().saturating_sub(WINDOW_SAMPLES)..];
    let mut x = vec![0.0f32; WINDOW_SAMPLES - tail.len()];
    x.extend_from_slice(tail);
    normalize(&mut x);
    whisper_features(&x, MEL_FRAMES, 0)
}

/// Whisper's log-mel features of the whole clip, one frame per 10 ms, followed by `pad` frames of
/// the clip's floor. Returns `MEL_BINS × (frames + pad)`, mel-major, and the frame count with the pad.
pub fn whisper_log_mel(samples_16k: &[f32], pad: usize) -> (Vec<f32>, usize) {
    let frames = samples_16k.len() / HOP;
    (whisper_features(samples_16k, frames, pad), frames + pad)
}

fn whisper_features(x: &[f32], frames: usize, pad: usize) -> Vec<f32> {
    let power = power_spectrogram(x, frames);
    let filters = mel_filters();
    let width = frames + pad;
    let mut out = vec![0.0f32; MEL_BINS * width];
    for (bin, filter) in filters.iter().enumerate() {
        for (frame, spectrum) in power.iter().enumerate() {
            let energy: f64 = filter.iter().zip(spectrum).map(|(w, p)| w * p).sum();
            out[bin * width + frame] = energy.max(1e-10).log10() as f32;
        }
    }
    let peak = (0..MEL_BINS)
        .flat_map(|bin| &out[bin * width..bin * width + frames])
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let floor = peak - 8.0;
    for bin in 0..MEL_BINS {
        let row = &mut out[bin * width..(bin + 1) * width];
        for v in &mut row[..frames] {
            *v = (v.max(floor) + 4.0) / 4.0;
        }
        row[frames..].fill((floor + 4.0) / 4.0);
    }
    out
}

fn normalize(x: &mut [f32]) {
    let n = x.len() as f64;
    let mean = x.iter().map(|&v| f64::from(v)).sum::<f64>() / n;
    let var = x
        .iter()
        .map(|&v| (f64::from(v) - mean).powi(2))
        .sum::<f64>()
        / n;
    let scale = (var + 1e-7).sqrt();
    for v in x {
        *v = ((f64::from(*v) - mean) / scale) as f32;
    }
}

/// Centred, reflect-padded STFT power, `frames` frames (the trailing frame dropped).
fn power_spectrogram(x: &[f32], frames: usize) -> Vec<[f64; FREQ_BINS]> {
    let pad = N_FFT / 2;
    let n = x.len();
    let padded: Vec<f64> = (0..n + 2 * pad)
        .map(|i| {
            f64::from(match i {
                i if i < pad => x[pad - i],
                i if i >= n + pad => x[2 * n + pad - 2 - i],
                i => x[i - pad],
            })
        })
        .collect();

    let window = hann_window();
    let fft = FftPlanner::<f64>::new().plan_fft_forward(N_FFT);
    let mut buf = vec![Complex::new(0.0f64, 0.0); N_FFT];
    (0..frames)
        .map(|frame| {
            let start = frame * HOP;
            for (k, c) in buf.iter_mut().enumerate() {
                *c = Complex::new(padded[start + k] * window[k], 0.0);
            }
            fft.process(&mut buf);
            let mut power = [0.0f64; FREQ_BINS];
            for (p, c) in power.iter_mut().zip(&buf) {
                *p = c.norm_sqr();
            }
            power
        })
        .collect()
}

fn hann_window() -> &'static [f64; N_FFT] {
    static WINDOW: OnceLock<[f64; N_FFT]> = OnceLock::new();
    WINDOW.get_or_init(|| {
        std::array::from_fn(|k| 0.5 - 0.5 * (2.0 * PI * k as f64 / N_FFT as f64).cos())
    })
}

fn hz_to_mel(f: f64) -> f64 {
    if f < 1000.0 {
        3.0 * f / 200.0
    } else {
        15.0 + 27.0 * (f / 1000.0).ln() / 6.4f64.ln()
    }
}

fn mel_to_hz(m: f64) -> f64 {
    if m < 15.0 {
        200.0 * m / 3.0
    } else {
        1000.0 * ((m - 15.0) * 6.4f64.ln() / 27.0).exp()
    }
}

/// Slaney-normalised triangular filters over 0–8000 Hz, one row per mel bin.
fn mel_filters() -> &'static [[f64; FREQ_BINS]] {
    static FILTERS: OnceLock<Vec<[f64; FREQ_BINS]>> = OnceLock::new();
    FILTERS.get_or_init(|| {
        let top = hz_to_mel(SAMPLE_RATE as f64 / 2.0);
        let hz: Vec<f64> = (0..MEL_BINS + 2)
            .map(|i| mel_to_hz(top * i as f64 / (MEL_BINS + 1) as f64))
            .collect();
        (0..MEL_BINS)
            .map(|i| {
                let scale = 2.0 / (hz[i + 2] - hz[i]);
                std::array::from_fn(|k| {
                    let f = (k * SAMPLE_RATE) as f64 / N_FFT as f64;
                    let down = (f - hz[i]) / (hz[i + 1] - hz[i]);
                    let up = (hz[i + 2] - f) / (hz[i + 2] - hz[i + 1]);
                    down.min(up).max(0.0) * scale
                })
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_whisper_feature_extractor() {
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/data/mel_ref.json")).unwrap();
        let x: Vec<f32> = (0..16000 * 3)
            .map(|i| {
                let t = f64::from(i) / 16000.0;
                (0.3 * (2.0 * PI * 440.0 * t).sin() + 0.1 * (2.0 * PI * 1250.0 * t).sin()) as f32
            })
            .collect();
        let m = log_mel(&x);
        assert_eq!(m.len(), MEL_BINS * MEL_FRAMES);
        for (k, frame) in reference["frames"].as_array().unwrap().iter().enumerate() {
            let f = frame.as_u64().unwrap() as usize;
            for bin in 0..MEL_BINS {
                let want = reference["values"][k][bin].as_f64().unwrap() as f32;
                let got = m[bin * MEL_FRAMES + f];
                assert!(
                    (got - want).abs() < 2e-3,
                    "frame {f} bin {bin}: {got} vs {want}"
                );
            }
        }
    }

    #[test]
    fn whisper_features_cover_the_clip_then_its_floor() {
        let x: Vec<f32> = (0..16000).map(|i| (f64::from(i) * 0.05).sin() as f32 * 0.2).collect();
        let (m, frames) = whisper_log_mel(&x, 30);
        assert_eq!(frames, 100 + 30);
        assert_eq!(m.len(), MEL_BINS * frames);
        let low = m.iter().copied().fold(f32::INFINITY, f32::min);
        for bin in 0..MEL_BINS {
            assert!(m[bin * frames + 100..(bin + 1) * frames].iter().all(|&v| v == low), "bin {bin}");
        }
    }

    #[test]
    fn longer_input_keeps_the_last_eight_seconds() {
        let mut x = vec![0.5f32; 16000 * 10];
        x[..16000 * 2].fill(0.0);
        assert_eq!(log_mel(&x), log_mel(&x[16000 * 2..]));
    }
}
