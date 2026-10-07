use std::f64::consts::PI;

const PEAK: f32 = 0.9;
const TAPS: f64 = 16.0;

/// Polyphase windowed-sinc resampling of a whole render.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to {
        return input.to_vec();
    }
    let g = gcd(from, to);
    let (up, down) = ((to / g) as usize, (from / g) as usize);
    let cutoff = (f64::from(to) / f64::from(from)).min(1.0) * 0.95;
    let half = (TAPS / cutoff).ceil() as isize;
    let phases: Vec<Vec<f32>> = (0..up)
        .map(|p| {
            (-half + 1..=half)
                .map(|j| {
                    let x = p as f64 / up as f64 - j as f64;
                    (cutoff * sinc(cutoff * x) * blackman(x / half as f64)) as f32
                })
                .collect()
        })
        .collect();
    let len = input.len() * up / down;
    (0..len)
        .map(|i| {
            let (centre, phase) = ((i * down / up) as isize, i * down % up);
            phases[phase]
                .iter()
                .zip(centre - half + 1..)
                .filter(|(_, k)| *k >= 0 && (*k as usize) < input.len())
                .map(|(w, k)| w * input[k as usize])
                .sum()
        })
        .collect()
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// s16le PCM, peak-normalised so every render and both engines play at one level.
pub fn to_pcm(samples: &[f32]) -> Vec<i16> {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let gain = if peak > 1e-4 { PEAK / peak } else { 1.0 };
    samples
        .iter()
        .map(|s| (s * gain * 32767.0).clamp(-32768.0, 32767.0) as i16)
        .collect()
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

fn blackman(x: f64) -> f64 {
    if x.abs() >= 1.0 {
        return 0.0;
    }
    let p = PI * (x + 1.0);
    0.42 - 0.5 * p.cos() + 0.08 * (2.0 * p).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f64, secs: f64) -> Vec<f32> {
        (0..(f64::from(rate) * secs) as usize)
            .map(|i| (2.0 * PI * hz * i as f64 / f64::from(rate)).sin() as f32)
            .collect()
    }

    #[test]
    fn resampling_keeps_a_tone() {
        for from in [24000, 44100] {
            let out = resample(&tone(from, 440.0, 0.5), from, 48000);
            assert_eq!(out.len(), 24000);
            let want = tone(48000, 440.0, 0.5);
            let err = out[1000..23000]
                .iter()
                .zip(&want[1000..23000])
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(err < 0.01, "{from}: {err}");
        }
    }

    #[test]
    fn pcm_is_peak_normalised() {
        let pcm = to_pcm(&[0.0, 0.25, -0.5]);
        assert_eq!(pcm[2], -(0.9f32 * 32767.0) as i16);
    }
}
