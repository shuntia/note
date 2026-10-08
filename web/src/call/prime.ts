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

export function releaseAudioContext(): void {
  const ctx = primed
  primed = null
  void ctx?.close().catch(() => {})
}
