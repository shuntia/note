import gsap from 'gsap'
import { useCallback, useEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { useEscape } from './escape'
import { reducedMotion } from './motion'
import type { VoiceChoice } from './types'
import type { previewPlayer } from './voicePreview'

export function VoiceGrid({
  voices,
  chosen,
  playing,
  onPick,
}: {
  voices: VoiceChoice[]
  chosen: string
  playing: string | null
  onPick: (voice: string) => void
}) {
  return (
    <div className="voice-grid" role="group" aria-label="Voice">
      {voices.map((v) => (
        <button
          key={v.id}
          type="button"
          className="voice-tile"
          aria-pressed={chosen === v.id}
          data-playing={playing === v.id || undefined}
          onClick={() => onPick(v.id)}
        >
          <span className="voice-name">{v.label}</span>
          <span className="voice-bars" aria-hidden="true">
            <i />
            <i />
            <i />
          </span>
        </button>
      ))}
    </div>
  )
}

export function VoiceSheet({
  voices,
  chosen,
  preview,
  onPick,
  onClose,
}: {
  voices: VoiceChoice[]
  chosen: string
  preview: ReturnType<typeof previewPlayer>
  onPick: (voice: string) => void
  onClose: () => void
}) {
  const [playing, setPlaying] = useState<string | null>(null)
  const scrim = useRef<HTMLDivElement>(null)
  const panel = useRef<HTMLDivElement>(null)
  const closing = useRef(false)

  useEffect(() => {
    if (reducedMotion()) return
    gsap.from(scrim.current, { autoAlpha: 0, duration: 0.3, ease: 'power2.out' })
    gsap.from(panel.current, { y: 48, autoAlpha: 0, duration: 0.4, ease: 'expo.out', clearProps: 'transform,opacity,visibility' })
  }, [])

  const close = useCallback(() => {
    if (closing.current) return
    closing.current = true
    preview.stop()
    if (reducedMotion()) return onClose()
    gsap.to(scrim.current, { autoAlpha: 0, duration: 0.25, ease: 'power2.in' })
    gsap.to(panel.current, { y: 48, autoAlpha: 0, duration: 0.28, ease: 'power2.in', onComplete: onClose })
  }, [onClose, preview])
  useEscape(true, close)

  const pick = (voice: string) => {
    setPlaying(voice)
    preview.play(voice, () => setPlaying((p) => (p === voice ? null : p)))
    onPick(voice)
  }

  return createPortal(
    <>
      <div className="scrim" ref={scrim} onClick={() => close()} />
      <div className="sheet voice-sheet" role="dialog" aria-modal="true" aria-label="Voice" ref={panel}>
        <div className="sheet-handle" />
        <VoiceGrid voices={voices} chosen={chosen} playing={playing} onPick={pick} />
      </div>
    </>,
    document.body,
  )
}
