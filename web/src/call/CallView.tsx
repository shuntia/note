import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react'
import { t } from '../i18n'
import { reducedMotion } from '../motion'
import { onCallFrame } from '../ws'
import { CallControl, type CallOpen, type CallShown, type Phase } from './control'
import { ease, shape, smooth, stillForm, target, type Form, type Look, type Pt } from './form'
import { primeAudio, releaseAudioContext } from './prime'
import { callUrl, CallSession, type AudioIo, type LiveState } from './session'
import '../styles/call.css'

export type { CallOpen }

const SWIPE_PX = 80
const MAX_FRAME_MS = 100
const ALPHA_STEPS = 64

function lookOf(phase: Phase, live: LiveState): Look {
  if (phase === 'ringing') return 'ringing'
  if (phase === 'gone') return 'ending'
  if (phase === 'connecting') return 'listening'
  return live
}

// Strokes the form as runs of segments sharing one quantised alpha, so fading ink stays continuous without overlapping caps.
function drawForm(g: CanvasRenderingContext2D, pts: Pt[], scale: number, base: number) {
  const alphaAt = (i: number) => Math.round(((pts[i - 1].a + pts[i].a) / 2) * base * ALPHA_STEPS) / ALPHA_STEPS
  let i = 1
  while (i < pts.length) {
    const a = alphaAt(i)
    let j = i
    while (j + 1 < pts.length && alphaAt(j + 1) === a) j++
    if (a > 0) {
      g.globalAlpha = a
      g.beginPath()
      g.moveTo(pts[i - 1].x * scale, pts[i - 1].y * scale)
      for (let k = i; k <= j; k++) g.lineTo(pts[k].x * scale, pts[k].y * scale)
      g.stroke()
    }
    i = j + 1
  }
  g.globalAlpha = 1
}

export function CallView({
  call,
  onClose,
  onMicBlocked,
}: {
  call: CallOpen
  onClose: (conversationId: number | null) => void
  onMicBlocked: () => void
}) {
  const props = useRef({ onClose, onMicBlocked })
  props.current = { onClose, onMicBlocked }
  const [control] = useState(() => {
    let audio: () => AudioIo
    return new CallControl(
      call,
      {
        load: async () => {
          audio = (await import('./audio')).webAudio
        },
        connect: (events) => new CallSession(callUrl(location, call), audio(), events),
        prime: primeAudio,
        release: releaseAudioContext,
        onClose: (id) => props.current.onClose(id),
        onMicBlocked: () => props.current.onMicBlocked(),
        later: (fn, ms) => window.setTimeout(fn, ms),
        cancel: (id) => window.clearTimeout(id),
      },
      (next) => setShown(next),
    )
  })
  const [shown, setShown] = useState<CallShown>(control.shown)
  const { phase, live, muted, caption, shake } = shown
  const canvas = useRef<HTMLCanvasElement>(null)
  const root = useRef<HTMLDivElement>(null)
  const swipeFrom = useRef<number | null>(null)
  const swiped = useRef(false)
  const look = lookOf(phase, live)
  const drawn = useRef({ look, muted })
  drawn.current = { look, muted }

  const tap = () => {
    if (swiped.current) {
      swiped.current = false
      return
    }
    control.tap()
  }

  const down = (e: PointerEvent) => {
    swiped.current = false
    swipeFrom.current = e.clientY
  }
  const up = (e: PointerEvent) => {
    const from = swipeFrom.current
    swipeFrom.current = null
    if (from !== null && e.clientY - from > SWIPE_PX) {
      swiped.current = true
      control.end()
    }
  }

  const key = (e: KeyboardEvent) => {
    if (e.key !== 'Tab' || !root.current) return
    const stops = [...root.current.querySelectorAll('button')]
    const at = stops.indexOf(document.activeElement as HTMLButtonElement)
    const step = e.shiftKey ? stops.length - 1 : 1
    e.preventDefault()
    stops[at < 0 ? (e.shiftKey ? stops.length - 1 : 0) : (at + step) % stops.length]?.focus()
  }

  useEffect(() => control.mount(), [control])
  useEffect(() => {
    const escape = (e: globalThis.KeyboardEvent) => {
      if (e.key !== 'Escape') return
      e.preventDefault()
      e.stopPropagation()
      control.end()
    }
    window.addEventListener('keydown', escape, true)
    return () => window.removeEventListener('keydown', escape, true)
  }, [control])
  useEffect(() => () => control.dispose(), [control])
  useEffect(
    () =>
      onCallFrame((f) => {
        if (f.type === 'ring_taken') control.ringTaken(f.ring)
      }),
    [control],
  )

  useEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null
    root.current?.focus()
    return () => opener?.focus()
  }, [])

  useEffect(() => {
    const el = canvas.current
    const g = el?.getContext('2d')
    if (!el || !g) return
    const still = reducedMotion()
    let form: Form = target(drawn.current.look, drawn.current.muted)
    let mic = 0
    let out = 0
    let clock = 0
    let last = performance.now()
    const ink = () => getComputedStyle(el).getPropertyValue('--ink').trim() || 'currentColor'
    let color = ink()
    let colorAt = 0
    let frame = 0
    const draw = (now: number) => {
      const dt = Math.max(0, Math.min(MAX_FRAME_MS, now - last))
      last = now
      clock += dt
      const lv = control.levels()
      mic = smooth(mic, Math.min(1, lv.mic * 4), dt)
      out = smooth(out, Math.min(1, lv.out * 4), dt)
      const { look, muted } = drawn.current
      form = still ? stillForm(look, muted) : ease(form, target(look, muted), dt)
      if (clock - colorAt > 1000) {
        color = ink()
        colorAt = clock
      }
      const dpr = window.devicePixelRatio || 1
      const size = Math.round(el.clientWidth * dpr)
      if (el.width !== size) el.width = el.height = size
      g.setTransform(1, 0, 0, 1, 0, 0)
      g.clearRect(0, 0, size, size)
      g.translate(size / 2, size / 2)
      g.lineWidth = 2.2 * dpr
      g.lineCap = 'butt'
      g.lineJoin = 'round'
      g.strokeStyle = color
      drawForm(g, shape(form, still ? 0 : clock, { mic, out }), (size / 2) * 0.62, 1 - 0.45 * form.dim)
      frame = requestAnimationFrame(draw)
    }
    frame = requestAnimationFrame(draw)
    return () => cancelAnimationFrame(frame)
  }, [])

  const state = phase === 'live' || phase === 'hanging' ? live : phase

  return (
    <div
      ref={root}
      tabIndex={-1}
      className={`call-view${shake ? ' shake' : ''}${phase === 'gone' ? ' gone' : ''}`}
      data-state={state}
      data-muted={muted}
      role="dialog"
      aria-modal="true"
      aria-label={t('talk.call')}
      onPointerDown={down}
      onPointerUp={up}
      onKeyDown={key}
    >
      <button className="call-close" aria-label={t('call.end')} onClick={() => control.end()}>
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M6 6l12 12" />
          <path d="M18 6L6 18" />
        </svg>
      </button>
      <button
        className="call-circle"
        aria-label={phase === 'ringing' ? t('call.answer') : muted ? t('call.unmute') : t('call.mute')}
        aria-pressed={phase === 'ringing' ? undefined : muted}
        onClick={tap}
      >
        <canvas ref={canvas} aria-hidden="true" />
      </button>
      <div className="call-captions" aria-live="polite">
        {caption && (
          <p key={caption.n} className="call-caption">
            {caption.text}
          </p>
        )}
      </div>
    </div>
  )
}
