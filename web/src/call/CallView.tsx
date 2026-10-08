import { useCallback, useEffect, useRef, useState, type PointerEvent } from 'react'
import { t } from '../i18n'
import { reducedMotion } from '../motion'
import { onCallFrame } from '../ws'
import { ease, shape, smooth, stillForm, target, type Form, type Look, type Pt } from './form'
import { primeAudio, releaseAudioContext } from './prime'
import { callUrl, CallSession, type LiveState } from './session'
import '../styles/call.css'

export type CallOpen = { conversationId: number | null; ring: string | null; at: number }

type Phase = 'ringing' | 'connecting' | 'live' | 'hanging' | 'gone'
type Run = { alive: boolean; session: CallSession | null }

const SWIPE_PX = 80
const LEAVE_MS = 450
const RING_MS = 30_000
// Past the voice side's longest drain, so Note's last words always finish first.
const HANG_UP_WAIT_MS = 62_000
const MAX_FRAME_MS = 100
const ALPHA_STEPS = 64
const FAILED = new Set(['unavailable', 'busy', 'failed', 'missed'])

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
  const [phase, setPhase] = useState<Phase>(call.ring ? 'ringing' : 'connecting')
  const [live, setLive] = useState<LiveState>('listening')
  const [muted, setMuted] = useState(false)
  const [caption, setCaption] = useState<{ text: string; at: number } | null>(null)
  const [shake, setShake] = useState(false)
  const run = useRef<Run | null>(null)
  const timers = useRef<number[]>([])
  const canvas = useRef<HTMLCanvasElement>(null)
  const root = useRef<HTMLDivElement>(null)
  const swipeFrom = useRef<number | null>(null)
  const swiped = useRef(false)
  const phaseRef = useRef(phase)
  phaseRef.current = phase
  const look = lookOf(phase, live)
  const drawn = useRef({ look, muted })
  drawn.current = { look, muted }

  const later = useCallback((fn: () => void, ms: number) => {
    timers.current.push(window.setTimeout(fn, ms))
  }, [])

  const stop = useCallback(() => {
    const r = run.current
    if (!r) return
    r.alive = false
    r.session?.close()
  }, [])

  // The call's one true end: the audio context primed by the opening tap goes with it.
  const leave = useCallback(
    (conversationId: number | null, failed: boolean) => {
      if (phaseRef.current === 'gone') return
      phaseRef.current = 'gone'
      setPhase('gone')
      setShake(failed)
      releaseAudioContext()
      later(() => onClose(conversationId), LEAVE_MS)
    },
    [later, onClose],
  )

  const begin = useCallback(async () => {
    const token: Run = { alive: true, session: null }
    run.current = token
    const { webAudio } = await import('./audio')
    if (!token.alive) return
    const s = new CallSession(callUrl(location, call), webAudio(), {
      state: (state) => {
        setLive(state)
        setPhase((p) => (p === 'connecting' ? 'live' : p))
      },
      caption: (text) => setCaption({ text, at: Date.now() }),
      ended: (e) => leave(e.conversationId, FAILED.has(e.reason)),
      micDenied: () => {
        onMicBlocked()
        leave(null, true)
      },
    })
    token.session = s
    await s.start()
  }, [call, leave, onMicBlocked])

  const end = useCallback(() => {
    const p = phaseRef.current
    if (p === 'ringing' || p === 'connecting') {
      stop()
      return leave(null, false)
    }
    if (p !== 'live') return
    phaseRef.current = 'hanging'
    setPhase('hanging')
    run.current?.session?.hangUp()
    later(() => {
      stop()
      leave(call.conversationId, false)
    }, HANG_UP_WAIT_MS)
  }, [call.conversationId, later, leave, stop])

  const answer = () => {
    primeAudio()
    phaseRef.current = 'connecting'
    setPhase('connecting')
    void begin()
  }

  const tap = () => {
    if (swiped.current) {
      swiped.current = false
      return
    }
    const p = phaseRef.current
    if (p === 'ringing') return answer()
    if (p !== 'live' && p !== 'connecting') return
    const on = !muted
    setMuted(on)
    run.current?.session?.setMuted(on)
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
      end()
    }
  }

  useEffect(() => {
    if (call.ring) {
      const timer = window.setTimeout(() => {
        if (phaseRef.current === 'ringing') leave(null, false)
      }, RING_MS)
      return () => {
        window.clearTimeout(timer)
        stop()
      }
    }
    void begin()
    return stop
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  useEffect(() => () => timers.current.forEach((id) => window.clearTimeout(id)), [])

  useEffect(
    () =>
      onCallFrame((f) => {
        if (f.type === 'ring_taken' && f.ring === call.ring && phaseRef.current === 'ringing') leave(null, false)
      }),
    [call.ring, leave],
  )

  useEffect(() => root.current?.focus(), [])

  const endRef = useRef(end)
  endRef.current = end
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') endRef.current()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
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
      const lv = run.current?.session?.levels() ?? { mic: 0, out: 0 }
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

  const shown = phase === 'live' || phase === 'hanging' ? live : phase

  return (
    <div
      ref={root}
      tabIndex={-1}
      className={`call-view${shake ? ' shake' : ''}${phase === 'gone' ? ' gone' : ''}`}
      data-state={shown}
      data-muted={muted}
      role="dialog"
      aria-modal="true"
      aria-label={t('talk.call')}
      onPointerDown={down}
      onPointerUp={up}
    >
      <button className="call-close" aria-label={t('call.end')} onClick={end}>
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
      {caption && (
        <p key={caption.at} className="call-caption" aria-live="polite">
          {caption.text}
        </p>
      )}
    </div>
  )
}
