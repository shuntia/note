export type Sample = Pick<HTMLAudioElement, 'src' | 'play' | 'pause'>

export const previewUrl = (voice: string) => `/api/voice/preview?voice=${encodeURIComponent(voice)}`

// Plays one voice's sample at a time through a single element, made on the first play.
export function previewPlayer(make: () => Sample = () => new Audio()) {
  let audio: Sample | null = null
  return (voice: string) => {
    if (audio) audio.pause()
    else audio = make()
    audio.src = previewUrl(voice)
    void audio.play().catch(() => undefined)
  }
}
