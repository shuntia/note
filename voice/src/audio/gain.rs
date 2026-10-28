use note_voice_proto::MicGain;

const RATE: f32 = 16_000.0;
pub const TARGET_DB: f32 = -20.0;
pub const MAX_GAIN_DB: f32 = 24.0;
/// How fast the gain may climb toward a quiet speaker, and fall back for a loud one.
const RISE_DB_PER_S: f32 = 3.0;
const FALL_DB_PER_S: f32 = 12.0;
/// The time constant of the speech level, over speech only.
const LEVEL_TAU_S: f32 = 1.0;
/// How fast the noise floor creeps up; it drops at once to a quieter frame.
const FLOOR_RISE_DB_PER_S: f32 = 1.5;
/// A frame this far over the noise floor, and over `SPEECH_MIN_DB`, counts as speech.
const SPEECH_OVER_FLOOR_DB: f32 = 12.0;
const SPEECH_MIN_DB: f32 = -65.0;
/// A frame that is not speech is raised no louder than this, below the recognizer's quiet line.
const NOISE_CEIL_DB: f32 = -52.0;
/// Samples are held under `CEILING`; past it they bend softly toward `FULL`.
const CEILING: f32 = 0.89;
const FULL: f32 = 0.99;

fn db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

fn lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// A call's input gain, ahead of VAD, turn detection and STT. In auto it tracks the speech level
/// toward `TARGET_DB` RMS, moving only on speech so pauses neither pump nor raise the noise.
pub struct Agc {
    gain_db: f32,
    auto: bool,
    level_db: Option<f32>,
    floor_db: f32,
    applied: f32,
}

impl Agc {
    pub fn new(mic: MicGain) -> Self {
        let gain_db = sane(mic.gain_db);
        Agc { gain_db, auto: mic.auto, level_db: None, floor_db: -70.0, applied: lin(gain_db) }
    }

    pub fn settings(&self) -> MicGain {
        MicGain { gain_db: (self.gain_db * 10.0).round() / 10.0, auto: self.auto }
    }

    /// A manual gain holds still; turning auto back on adapts from it.
    pub fn set(&mut self, mic: MicGain) {
        self.gain_db = sane(mic.gain_db);
        self.auto = mic.auto;
    }

    /// `speech`: the VAD's latest verdict.
    pub fn process(&mut self, frame: &mut [f32], speech: bool) {
        if frame.is_empty() {
            return;
        }
        let dt = frame.len() as f32 / RATE;
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();
        let peak = frame.iter().fold(0f32, |m, s| m.max(s.abs()));
        let level = db(rms);
        self.floor_db = if level < self.floor_db { level } else { self.floor_db + FLOOR_RISE_DB_PER_S * dt };
        let speechlike = level > SPEECH_MIN_DB && (speech || level > self.floor_db + SPEECH_OVER_FLOOR_DB);
        if self.auto && speechlike {
            let smoothed = match self.level_db {
                Some(prev) => prev + (level - prev) * (dt / LEVEL_TAU_S).min(1.0),
                None => level,
            };
            self.level_db = Some(smoothed);
            let want = (TARGET_DB - smoothed).clamp(0.0, MAX_GAIN_DB);
            let step = (want - self.gain_db).clamp(-FALL_DB_PER_S * dt, RISE_DB_PER_S * dt);
            self.gain_db += step;
        }
        let mut gain_db = self.gain_db;
        if !speechlike {
            gain_db = gain_db.min((NOISE_CEIL_DB - level).max(0.0));
        }
        let mut target = lin(gain_db);
        if peak * target > CEILING {
            target = target.min((CEILING / peak).max(1.0));
        }
        let from = self.applied;
        let n = frame.len() as f32;
        for (i, s) in frame.iter_mut().enumerate() {
            let g = from + (target - from) * ((i + 1) as f32 / n);
            *s = soft_limit(*s * g);
        }
        self.applied = target;
    }
}

fn sane(gain_db: f32) -> f32 {
    if gain_db.is_finite() { gain_db.clamp(0.0, MAX_GAIN_DB) } else { 0.0 }
}

fn soft_limit(x: f32) -> f32 {
    let a = x.abs();
    if a <= CEILING {
        return x;
    }
    let room = FULL - CEILING;
    x.signum() * (CEILING + room * ((a - CEILING) / room).tanh())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: usize = 320;

    fn rms_db(x: &[f32]) -> f32 {
        db((x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt())
    }

    /// A 220 Hz tone at `level_db` RMS, in syllable-like bursts of `on` frames out of every `period`.
    fn speech(seconds: f32, level_db: f32, on: usize, period: usize) -> Vec<Vec<f32>> {
        let amp = lin(level_db) * std::f32::consts::SQRT_2;
        let frames = (seconds * RATE) as usize / FRAME;
        (0..frames)
            .map(|f| {
                let voiced = f % period < on;
                (0..FRAME)
                    .map(|i| {
                        let t = (f * FRAME + i) as f32 / RATE;
                        if voiced { amp * (std::f32::consts::TAU * 220.0 * t).sin() } else { 0.0 }
                    })
                    .collect()
            })
            .collect()
    }

    fn noise(seconds: f32, level_db: f32) -> Vec<Vec<f32>> {
        let mut x = 0x2545_f491_u32;
        let amp = lin(level_db) * 3f32.sqrt();
        let frames = (seconds * RATE) as usize / FRAME;
        (0..frames)
            .map(|_| {
                (0..FRAME)
                    .map(|_| {
                        x ^= x << 13;
                        x ^= x >> 17;
                        x ^= x << 5;
                        amp * (x as f32 / u32::MAX as f32 * 2.0 - 1.0)
                    })
                    .collect()
            })
            .collect()
    }

    fn run(agc: &mut Agc, frames: Vec<Vec<f32>>, speech: bool) -> Vec<Vec<f32>> {
        frames
            .into_iter()
            .map(|mut f| {
                let voiced = speech && f.iter().any(|s| *s != 0.0);
                agc.process(&mut f, voiced);
                f
            })
            .collect()
    }

    fn auto() -> Agc {
        Agc::new(MicGain { gain_db: 0.0, auto: true })
    }

    #[test]
    fn a_quiet_speaker_is_raised_to_the_target() {
        let mut agc = auto();
        let out = run(&mut agc, speech(20.0, -38.0, 1, 1), true);
        assert!((agc.gain_db - 18.0).abs() < 1.0, "{}", agc.gain_db);
        assert!((rms_db(out.last().unwrap()) - TARGET_DB).abs() < 1.0);
    }

    #[test]
    fn the_gain_stays_within_unity_and_24_db() {
        let mut faint = auto();
        run(&mut faint, speech(30.0, -60.0, 1, 1), true);
        assert!((faint.gain_db - MAX_GAIN_DB).abs() < 1e-3, "{}", faint.gain_db);
        let mut loud = Agc::new(MicGain { gain_db: 12.0, auto: true });
        run(&mut loud, speech(5.0, -8.0, 1, 1), true);
        assert!(loud.gain_db.abs() < 1e-3, "{}", loud.gain_db);
    }

    #[test]
    fn silence_and_room_noise_are_not_raised() {
        let mut agc = auto();
        run(&mut agc, noise(10.0, -70.0), false);
        assert_eq!(agc.gain_db, 0.0);
        let mut learned = Agc::new(MicGain { gain_db: MAX_GAIN_DB, auto: true });
        let out = run(&mut learned, noise(5.0, -62.0), false);
        assert_eq!(learned.gain_db, MAX_GAIN_DB);
        assert!(out.iter().skip(5).all(|f| rms_db(f) <= NOISE_CEIL_DB + 0.5), "noise stays under the quiet line");
        let mut digital = Agc::new(MicGain { gain_db: MAX_GAIN_DB, auto: true });
        let zeros = run(&mut digital, vec![vec![0.0; FRAME]; 50], false);
        assert!(zeros.iter().flatten().all(|s| *s == 0.0));
    }

    #[test]
    fn pauses_do_not_move_the_gain() {
        let mut agc = auto();
        run(&mut agc, speech(6.0, -36.0, 1, 1), true);
        let before = agc.gain_db;
        run(&mut agc, speech(3.0, -36.0, 0, 1), true);
        run(&mut agc, noise(3.0, -75.0), false);
        assert_eq!(agc.gain_db, before);
    }

    #[test]
    fn nothing_clips() {
        let mut agc = Agc::new(MicGain { gain_db: MAX_GAIN_DB, auto: true });
        let out = run(&mut agc, speech(2.0, -6.0, 3, 5), true);
        assert!(out.iter().flatten().all(|s| s.abs() < 1.0));
        let mut fixed = Agc::new(MicGain { gain_db: MAX_GAIN_DB, auto: false });
        let out = run(&mut fixed, speech(1.0, -20.0, 1, 1), true);
        assert!(out.iter().flatten().all(|s| s.abs() < 1.0));
    }

    #[test]
    fn a_manual_gain_holds_and_auto_resumes_from_it() {
        let mut agc = Agc::new(MicGain { gain_db: 6.0, auto: false });
        let out = run(&mut agc, speech(10.0, -40.0, 1, 1), true);
        assert_eq!(agc.settings(), MicGain { gain_db: 6.0, auto: false });
        assert!((rms_db(out.last().unwrap()) - (-34.0)).abs() < 0.5);
        agc.set(MicGain { gain_db: 6.0, auto: true });
        run(&mut agc, speech(1.0, -40.0, 1, 1), true);
        assert!(agc.gain_db > 6.0 && agc.gain_db <= 6.0 + RISE_DB_PER_S + 0.01, "{}", agc.gain_db);
    }

    #[test]
    fn a_gain_that_is_not_a_number_is_unity() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(Agc::new(MicGain { gain_db: bad, auto: true }).settings().gain_db, 0.0);
            let mut agc = Agc::new(MicGain { gain_db: 6.0, auto: false });
            agc.set(MicGain { gain_db: bad, auto: false });
            let mut f = vec![0.1; FRAME];
            agc.process(&mut f, true);
            assert!(f.iter().all(|s| s.is_finite()));
            assert_eq!(agc.settings().gain_db, 0.0);
        }
    }

    #[test]
    fn a_gain_change_ramps_across_the_frame() {
        let mut agc = Agc::new(MicGain { gain_db: 0.0, auto: false });
        agc.set(MicGain { gain_db: 20.0, auto: false });
        let mut f = vec![0.01; FRAME];
        agc.process(&mut f, true);
        assert!(f[0] < 0.011 && (f[FRAME - 1] - 0.1).abs() < 1e-3);
    }
}
