import connectUrl from '../sounds/connect.mp3'
import hangupUrl from '../sounds/hangup.mp3'
import noticeUrl from '../sounds/notice.mp3'
import ringUrl from '../sounds/ring.mp3'
import type { Cue } from './control'
import { takeAudioContext } from './prime'

// A failed call ends without a sound.
export const CUES: Record<Cue, string | null> = { connect: connectUrl, hangup: hangupUrl, failed: null }

// How long the primed context must outlive a cue that sounds as the call ends: the hang-up file runs 0.51 s.
export const CUE_TAIL_MS = 700

const FADE_S = 0.04
const decoded = new WeakMap<AudioContext, Map<string, Promise<AudioBuffer>>>()

function buffer(ctx: AudioContext, url: string): Promise<AudioBuffer> {
  let mine = decoded.get(ctx)
  if (!mine) decoded.set(ctx, (mine = new Map()))
  let b = mine.get(url)
  if (!b) {
    b = fetch(url)
      .then((r) => r.arrayBuffer())
      .then((data) => ctx.decodeAudioData(data))
    b.catch(() => mine.delete(url))
    mine.set(url, b)
  }
  return b
}

// Plays `url` on `ctx` through a gain that `stop` fades out; a refused or failed playback stays silent.
function play(ctx: AudioContext, url: string, loop: boolean): () => void {
  let stopped = false
  const out = ctx.createGain()
  out.connect(ctx.destination)
  void ctx.resume().catch(() => {})
  void buffer(ctx, url)
    .then((b) => {
      if (stopped) return
      const src = ctx.createBufferSource()
      src.buffer = b
      src.loop = loop
      src.connect(out)
      src.start()
    })
    .catch(() => {})
  return () => {
    stopped = true
    try {
      out.gain.setTargetAtTime(0, ctx.currentTime, FADE_S / 3)
      window.setTimeout(() => out.disconnect(), FADE_S * 4000)
    } catch {}
  }
}

export function ringtone(): () => void {
  try {
    return play(takeAudioContext(), ringUrl, true)
  } catch {
    return () => {}
  }
}

export function chime(cue: Cue): void {
  const url = CUES[cue]
  if (!url) return
  try {
    play(takeAudioContext(), url, false)
  } catch {}
}

let noticeCtx: AudioContext | null = null

// The desktop app's sound for a notice; its own context, so a call's primed one is never held open by it.
export function notice(): void {
  try {
    noticeCtx ??= new AudioContext()
    play(noticeCtx, noticeUrl, false)
  } catch {}
}
