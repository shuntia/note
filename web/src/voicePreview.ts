export type Sample = Pick<HTMLAudioElement, 'src' | 'play' | 'pause' | 'onended'>

export const previewUrl = (voice: string) => `/api/voice/preview?voice=${encodeURIComponent(voice)}`

// Plays one voice's sample at a time through a single element, made on the first play.
export function previewPlayer(make: () => Sample = () => new Audio()) {
  let audio: Sample | null = null
  return {
    play(voice: string, onEnd?: () => void) {
      if (audio) audio.pause()
      else audio = make()
      audio.onended = () => onEnd?.()
      audio.src = previewUrl(voice)
      void audio.play().catch(() => onEnd?.())
    },
    stop() {
      audio?.pause()
    },
  }
}
