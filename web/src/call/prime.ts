let primed: AudioContext | null = null

// Called inside the tap that opens or answers a call, so the browser lets the context play.
export function primeAudio(): void {
  if (!primed || primed.state === 'closed') primed = new AudioContext({ latencyHint: 'interactive' })
  void primed.resume().catch(() => {})
}

export function takeAudioContext(): AudioContext {
  const ctx = primed && primed.state !== 'closed' ? primed : new AudioContext({ latencyHint: 'interactive' })
  primed = null
  return ctx
}
