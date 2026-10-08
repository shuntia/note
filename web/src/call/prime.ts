let primed: AudioContext | null = null

const live = () => (primed && primed.state !== 'closed' ? primed : null)

// Called inside the tap that opens or answers a call, so the browser lets the context play.
export function primeAudio(): void {
  primed = live() ?? new AudioContext({ latencyHint: 'interactive' })
  void primed.resume().catch(() => {})
}

// The same context until releaseAudioContext, so a remounted call view keeps the gesture-unlocked one.
export function takeAudioContext(): AudioContext {
  primed = live() ?? new AudioContext({ latencyHint: 'interactive' })
  return primed
}

// `lingerMs` lets a sound already scheduled on the context finish before it closes; the next take gets a fresh one at once.
export function releaseAudioContext(lingerMs = 0): void {
  const ctx = primed
  primed = null
  if (!ctx) return
  const close = () => void ctx.close().catch(() => {})
  if (lingerMs > 0) setTimeout(close, lingerMs)
  else close()
}
