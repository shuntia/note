import { RING_MS, type Cue } from './control'
import { takeAudioContext } from './prime'

// One sine note: start and length in seconds, `peak` in linear gain.
export type Note = { at: number; freq: number; dur: number; peak: number }

export const ATTACK_S = 0.012
const FLOOR = 0.0001
const FADE_S = 0.04

export const RING_PERIOD_S = 3
const RING_CYCLE: Note[] = [
  { at: 0, freq: 659.25, dur: 0.9, peak: 0.07 },
  { at: 0.26, freq: 987.77, dur: 1.3, peak: 0.055 },
]

export const CUES: Record<Cue, Note[]> = {
  connected: [
    { at: 0, freq: 523.25, dur: 0.3, peak: 0.06 },
    { at: 0.1, freq: 783.99, dur: 0.5, peak: 0.05 },
  ],
  ended: [
    { at: 0, freq: 783.99, dur: 0.28, peak: 0.06 },
    { at: 0.12, freq: 523.25, dur: 0.55, peak: 0.05 },
  ],
  failed: [
    { at: 0, freq: 440, dur: 0.3, peak: 0.06 },
    { at: 0.15, freq: 293.66, dur: 0.6, peak: 0.05 },
  ],
}

export function ringNotes(seconds: number): Note[] {
  const notes: Note[] = []
  for (let start = 0; start < seconds; start += RING_PERIOD_S) {
    for (const n of RING_CYCLE) notes.push({ ...n, at: start + n.at })
  }
  return notes
}

export const length = (notes: Note[]) => Math.max(0, ...notes.map((n) => n.at + n.dur))

// How long the primed context must outlive a cue that sounds as the call ends.
export const CUE_TAIL_MS = Math.ceil(Math.max(...Object.values(CUES).map(length)) * 1000) + 100

function sound(ctx: AudioContext, notes: Note[], start: number, out: AudioNode) {
  for (const n of notes) {
    const t = start + n.at
    const osc = ctx.createOscillator()
    const gain = ctx.createGain()
    osc.frequency.value = n.freq
    gain.gain.setValueAtTime(0, t)
    gain.gain.linearRampToValueAtTime(n.peak, t + ATTACK_S)
    gain.gain.exponentialRampToValueAtTime(FLOOR, t + n.dur)
    osc.connect(gain).connect(out)
    osc.start(t)
    osc.stop(t + n.dur + FADE_S)
  }
}

// A browser may refuse to start audio without a gesture; the ring then stays silent.
export function ringtone(): () => void {
  try {
    const ctx = takeAudioContext()
    void ctx.resume().catch(() => {})
    const out = ctx.createGain()
    out.connect(ctx.destination)
    sound(ctx, ringNotes(RING_MS / 1000), ctx.currentTime + 0.05, out)
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
    sound(ctx, CUES[cue], ctx.currentTime + 0.02, ctx.destination)
  } catch {}
}
