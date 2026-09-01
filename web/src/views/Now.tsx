import { useCallback, useEffect, useRef, useState, type CSSProperties, type TouchEvent } from 'react'
import { api } from '../api'
import type { ToastAction } from '../app'
import { NowCounter } from '../nowcounter'
import { eventLabel } from '../receipts'
import {
  effectiveStart,
  elapsedSec,
  readCounterMode,
  writeCounterMode,
  type CounterMode,
  type FocusSession,
} from '../session'
import type { PlanEvent } from '../types'

// The 240° sweep, out of the r=150 circle's 942.48 circumference.
const ARC = 628.32
const IDLE_MS = 30_000
const CROSSFADE_MS = 500
const SWIPE_PX = 40

const reduced = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches

function clock(at: Date): string {
  const day = at.toLocaleDateString(undefined, { weekday: 'long' })
  const time = `${String(at.getHours()).padStart(2, '0')}:${String(at.getMinutes()).padStart(2, '0')}`
  return `${day} · ${time}`
}

function wallMinutes(wall: string): number {
  const [h, m] = wall.split(':')
  return Number(h) * 60 + Number(m)
}

function nextEvent(events: PlanEvent[], at: Date): PlanEvent | null {
  const mins = at.getHours() * 60 + at.getMinutes()
  return (
    events.find(
      (ev) =>
        (ev.status === 'pending' || ev.status === 'snoozed') && wallMinutes(ev.wall_time) >= mins,
    ) ?? null
  )
}

function spoken(elapsed: number, durationSec: number | null): string {
  const m = Math.floor(elapsed / 60)
  const head = `${m} minute${m === 1 ? '' : 's'} elapsed`
  if (durationSec === null) return head
  return `${head} of ${Math.round(durationSec / 60)} minutes`
}

// The column is replaced rather than appended to server-side, so the text the
// session started with has to travel back out with the new line.
function withElapsedNote(previous: string, elapsed: number): string {
  const day = new Date().toISOString().slice(0, 10)
  const line = `${day} · focused ${Math.max(1, Math.round(elapsed / 60))} min`
  return previous.trim() ? `${previous.trim()}\n${line}` : line
}

export function Now({
  session,
  setSession,
  notify,
  onChanged,
  onLeave,
}: {
  session: FocusSession
  setSession: (s: FocusSession | null) => void
  notify: (msg: string, action?: ToastAction) => void
  onChanged: () => void
  onLeave: () => void
}) {
  const [mode, setMode] = useState<CounterMode>(readCounterMode)
  const [events, setEvents] = useState<PlanEvent[]>([])
  const [minute, setMinute] = useState(() => new Date())
  const [ambient, setAmbient] = useState(false)
  const [pinned, setPinned] = useState(false)
  const [sheet, setSheet] = useState(false)
  const [finishing, setFinishing] = useState(false)
  const [arcLen, setArcLen] = useState(0)
  const idle = useRef(0)
  const touchY = useRef<number | null>(null)

  useEffect(() => {
    api
      .planToday()
      .then(setEvents)
      .catch(() => setEvents([]))
  }, [])

  // Landing on the minute boundary keeps the header clock and the spoken label honest.
  useEffect(() => {
    let timer = 0
    const schedule = () => {
      timer = window.setTimeout(
        () => {
          setMinute(new Date())
          schedule()
        },
        60_000 - (Date.now() % 60_000) + 50,
      )
    }
    schedule()
    return () => window.clearTimeout(timer)
  }, [])

  // The counter owns the digits; this owns the sweep, clamped so overrun holds
  // the arc complete rather than wrapping.
  useEffect(() => {
    const total = session.durationSec
    if (total === null || total <= 0) return
    const paint = () => setArcLen(ARC * Math.min(1, elapsedSec(session) / total))
    paint()
    const id = setInterval(paint, 1000)
    return () => clearInterval(id)
  }, [session])

  const wake = useCallback(() => {
    if (pinned) return
    setAmbient(false)
    window.clearTimeout(idle.current)
    idle.current = window.setTimeout(() => setAmbient(true), IDLE_MS)
  }, [pinned])

  useEffect(() => {
    wake()
    const on = () => wake()
    document.addEventListener('pointermove', on)
    document.addEventListener('pointerdown', on)
    document.addEventListener('keydown', on)
    return () => {
      window.clearTimeout(idle.current)
      document.removeEventListener('pointermove', on)
      document.removeEventListener('pointerdown', on)
      document.removeEventListener('keydown', on)
    }
  }, [wake])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault()
        setSheet((open) => !open)
      } else if (e.key === 'ArrowUp') {
        e.preventDefault()
        setPinned(false)
        setAmbient(false)
      }
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [])

  const pause = () => setSession({ ...session, pausedAt: Date.now() })

  const resume = () =>
    setSession({
      ...session,
      pausedAt: null,
      pausedMs: session.pausedMs + (Date.now() - (session.pausedAt ?? Date.now())),
    })

  const flip = () => {
    const next: CounterMode = mode === 'elapsed' ? 'remaining' : 'elapsed'
    setMode(next)
    writeCounterMode(next)
  }

  const close = useCallback(() => {
    setSession(null)
    onLeave()
  }, [onLeave, setSession])

  // Ending writes what the agent needs to see and nothing the user has to answer for.
  const finish = (done: boolean) => {
    const elapsed = elapsedSec(session)
    if (session.taskId !== null) {
      const notes = withElapsedNote(session.notes, elapsed)
      api
        .patchTask(session.taskId, done ? { state: 'done', notes } : { notes })
        .then(onChanged)
        .catch(() => notify("Couldn't save the session. Try again."))
    } else if (done && session.eventId !== null) {
      api
        .eventAction(session.eventId, 'done')
        .then(onChanged)
        .catch(() => notify("Couldn't mark that done. Try again."))
    }
    if (!done || reduced()) {
      close()
      return
    }
    setFinishing(true)
    window.setTimeout(close, CROSSFADE_MS)
  }

  const onTouchStart = (e: TouchEvent) => {
    touchY.current = e.touches[0]?.clientY ?? null
  }

  const onTouchEnd = (e: TouchEvent) => {
    const from = touchY.current
    touchY.current = null
    const to = e.changedTouches[0]?.clientY
    if (from === null || to === undefined) return
    if (to - from > SWIPE_PX) {
      setPinned(true)
      setAmbient(true)
    } else if (from - to > SWIPE_PX) {
      setPinned(false)
      setAmbient(false)
    }
  }

  const paused = session.pausedAt !== null
  const next = nextEvent(events, minute)
  const hidden = ambient || pinned
  const elapsed = elapsedSec(session)
  const overrun = session.durationSec !== null && elapsed >= session.durationSec
  const arcStyle = { '--arc': String(arcLen) } as CSSProperties
  const mossStyle = { '--arc': String(session.durationSec === null ? ARC : arcLen) } as CSSProperties

  return (
    <div
      className={`now-screen${hidden ? ' ambient' : ''}${paused ? ' paused' : ''}${finishing ? ' finishing' : ''}`}
      onTouchStart={onTouchStart}
      onTouchEnd={onTouchEnd}
      onClick={() => {
        setPinned(false)
        setAmbient(false)
      }}
    >
      <header className="now-top">
        <span className="now-brand">Note</span>
        <span className="now-clock">{clock(minute)}</span>
      </header>

      <div className="now-stage">
        <div className="now-ring" role="group" aria-label={spoken(elapsed, session.durationSec)}>
          <svg className="now-gauge" viewBox="0 0 320 320" aria-hidden="true">
            <defs>
              <linearGradient id="now-sungrad" x1="0" y1="1" x2="1" y2="0">
                <stop offset="0" stopColor="#c96a08" />
                <stop offset="1" stopColor="#f6b053" />
              </linearGradient>
            </defs>
            <g transform="rotate(150 160 160)">
              <circle className="now-track" cx="160" cy="160" r="150" />
              {session.durationSec !== null && (
                <circle className="now-arc" cx="160" cy="160" r="150" style={arcStyle} />
              )}
              <circle className="now-moss" cx="160" cy="160" r="150" style={mossStyle} />
            </g>
          </svg>
          <div className={`now-center${overrun ? ' over' : ''}`}>
            <NowCounter
              startedAt={effectiveStart(session)}
              durationSec={session.durationSec ?? 0}
              mode={mode}
              pausedAt={session.pausedAt}
            />
            {(paused || session.durationSec !== null) && (
              <div className="now-denom">
                {paused ? 'paused' : `${Math.round((session.durationSec ?? 0) / 60)}m`}
              </div>
            )}
            <div className="now-task">{session.title}</div>
            {session.stepIndex !== null && (
              <div className="now-step">
                step {session.stepIndex} of {session.stepCount} · {session.stepName}
              </div>
            )}
          </div>
        </div>

        <button className="now-done" onClick={() => finish(true)}>
          Done
        </button>
        {next && (
          <div className="now-next">
            Next · {eventLabel(next.kind)}, {next.wall_time}
          </div>
        )}
      </div>

      <div className={`now-sheet${sheet ? ' open' : ''}`}>
        <button
          className="now-lip"
          aria-expanded={sheet}
          onClick={(e) => {
            e.stopPropagation()
            setSheet((open) => !open)
          }}
        >
          <span className="now-grab" aria-hidden="true" />
          <span className="now-hint">Break · End session</span>
        </button>
        {sheet && (
          <div className="now-sheet-body" onClick={(e) => e.stopPropagation()}>
            <button className="now-sheet-item" onClick={paused ? resume : pause}>
              {paused ? 'Back to it' : 'Take a break'}
            </button>
            <button className="now-sheet-item" onClick={() => finish(false)}>
              End session
            </button>
            <button className="now-sheet-item" onClick={flip}>
              Switch the number
            </button>
          </div>
        )}
      </div>
    </div>
  )
}
