import { RING_MS, type Cue } from './control'
import { takeAudioContext } from './prime'

// Overtone `ratio` × the note's frequency at `gain` × the note's gain.
export type Overtone = { ratio: number; gain: number }
// Times in seconds, gain linear; no partials is a pure sine.
export type Note = { freq: number; at: number; dur: number; gain: number; partials?: Overtone[] }
// A sound with a `period` repeats every that many seconds.
export type Sound = { notes: Note[]; period?: number }
export type SoundName = 'ring' | Cue
// One sine oscillator, expanded from a note and one of its partials.
export type Voice = { freq: number; at: number; dur: number; gain: number }

export const SOUNDS: Record<SoundName, Sound> = {
  ring: {
    period: 3,
    notes: [
      { freq: 659.25, at: 0, dur: 0.9, gain: 0.06, partials: [{ ratio: 2, gain: 0.15 }] },
      { freq: 987.77, at: 0.26, dur: 1.3, gain: 0.05, partials: [{ ratio: 2, gain: 0.1 }] },
    ],
  },
  connect: {
    notes: [
      { freq: 523.25, at: 0, dur: 0.3, gain: 0.06 },
      { freq: 783.99, at: 0.1, dur: 0.5, gain: 0.05 },
    ],
  },
  hangup: {
    notes: [
      { freq: 783.99, at: 0, dur: 0.28, gain: 0.06 },
      { freq: 523.25, at: 0.12, dur: 0.55, gain: 0.05 },
    ],
  },
  failed: {
    notes: [
      { freq: 440, at: 0, dur: 0.3, gain: 0.06 },
      { freq: 293.66, at: 0.15, dur: 0.6, gain: 0.05 },
    ],
  },
}

export const ATTACK_S = 0.012
const FLOOR = 0.0001
const FADE_S = 0.04

// The sound's notes over `seconds`: once, or every period that starts within it.
export function schedule(sound: Sound, seconds = 0): Note[] {
  if (!sound.period) return sound.notes
  const notes: Note[] = []
  for (let start = 0; start < seconds; start += sound.period) {
    for (const n of sound.notes) notes.push({ ...n, at: start + n.at })
  }
  return notes
}

export function voices(notes: Note[]): Voice[] {
  return notes.flatMap((n) => [
    { freq: n.freq, at: n.at, dur: n.dur, gain: n.gain },
    ...(n.partials ?? []).map((p) => ({ freq: n.freq * p.ratio, at: n.at, dur: n.dur, gain: n.gain * p.gain })),
  ])
}

export const length = (notes: Note[]) => Math.max(0, ...notes.map((n) => n.at + n.dur))

// How long the primed context must outlive a cue that sounds as the call ends.
export const CUE_TAIL_MS =
  Math.ceil(Math.max(...(['connect', 'hangup', 'failed'] as const).map((c) => length(SOUNDS[c].notes))) * 1000) + 100

function play(ctx: AudioContext, notes: Note[], start: number, out: AudioNode) {
  for (const v of voices(notes)) {
    const t = start + v.at
    const osc = ctx.createOscillator()
    const gain = ctx.createGain()
    osc.frequency.value = v.freq
    gain.gain.setValueAtTime(0, t)
    gain.gain.linearRampToValueAtTime(v.gain, t + ATTACK_S)
    gain.gain.exponentialRampToValueAtTime(FLOOR, t + v.dur)
    osc.connect(gain).connect(out)
    osc.start(t)
    osc.stop(t + v.dur + FADE_S)
  }
}

// A browser may refuse to start audio without a gesture; the ring then stays silent.
export function ringtone(): () => void {
  try {
    const ctx = takeAudioContext()
    void ctx.resume().catch(() => {})
    const out = ctx.createGain()
    out.connect(ctx.destination)
    play(ctx, schedule(SOUNDS.ring, RING_MS / 1000), ctx.currentTime + 0.05, out)
    return () => {
      try {
        out.gain.setTargetAtTime(0, ctx.currentTime, FADE_S / 3)
        window.setTimeout(() => out.disconnect(), FADE_S * 4000)
      } catch {}
    }
  } catch {
    return () => {}
  }
}

export function chime(cue: Cue): void {
  try {
    const ctx = takeAudioContext()
    void ctx.resume().catch(() => {})
    play(ctx, schedule(SOUNDS[cue]), ctx.currentTime + 0.02, ctx.destination)
  } catch {}
}
