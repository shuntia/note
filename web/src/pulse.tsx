import { useEffect, useState } from 'react'
import { api } from './api'
import { minutesOf } from './dayline'
import { eventFacts, nextUp } from './events'
import { readPrefs } from './prefs'
import { elapsedSec, type FocusSession } from './session'
import type { DayView, PlanEvent } from './types'

const VB = 32
const R = 13
const C = 2 * Math.PI * R
const FULL = C * (240 / 360)

const fmt = (s: number) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`

// Where the wait started: the end of the last settled routine before now, else 06:00.
function waitStart(events: PlanEvent[], now: number): number {
  const ended = events
    .filter((ev) => ev.status === 'done' || ev.status === 'dropped')
    .map((ev) => minutesOf(ev.end_wall_time ?? ev.wall_time))
    .filter((m) => m <= now)
  return ended.length ? Math.max(...ended) : 6 * 60
}

type Reading = { frac: number; text: string; over: boolean; faded: boolean }

function read(session: FocusSession | null, events: PlanEvent[]): Reading | null {
  if (session) {
    const total = session.durationSec
    const elapsed = elapsedSec(session)
    const over = total !== null && elapsed > total
    const shown =
      over && total !== null
        ? `+${fmt(elapsed - total)}`
        : fmt(total !== null && readPrefs().counter === 'remaining' ? total - elapsed : elapsed)
    return { frac: total ? Math.min(1, elapsed / total) : 0, text: shown, over, faded: false }
  }
  const d = new Date()
  const now = d.getHours() * 60 + d.getMinutes()
  const next = nextUp(events, now)
  if (!next) return null
  const { minutes } = eventFacts(next, now)
  if (minutes === null) return { frac: 1, text: 'now', over: false, faded: false }
  const from = waitStart(events, now)
  const to = minutesOf(next.wall_time)
  const frac = to <= from ? 1 : (now - from) / (to - from)
  return { frac, text: `${minutes} min`, over: false, faded: true }
}

// The session or the wait, as one small ring (a bar on desktop) with its
// countdown, for a view that is not Home; tapping it goes there.
export function Pulse({
  session,
  refresh,
  onOpen,
}: {
  session: FocusSession | null
  refresh: number
  onOpen: () => void
}) {
  const [day, setDay] = useState<DayView | null>(null)
  const [, tick] = useState(0)

  useEffect(() => {
    const load = () => {
      const d = new Date()
      const pad = (n: number) => String(n).padStart(2, '0')
      api
        .day(`${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`)
        .then(setDay)
        .catch(() => {})
    }
    load()
    const id = setInterval(load, 60_000)
    return () => clearInterval(id)
  }, [refresh])

  useEffect(() => {
    const id = setInterval(() => tick((n) => n + 1), session ? 1000 : 15_000)
    return () => clearInterval(id)
  }, [session])

  const reading = read(session, day?.events ?? [])
  if (!reading) return null
  const { frac, text, over, faded } = reading
  const prog = FULL * Math.max(0, Math.min(1, frac))
  return (
    <button
      className={`pulse${faded ? ' faded' : ''}${over ? ' over' : ''}`}
      aria-label={`${session ? 'Session' : 'Next'}: ${text}. Go home`}
      onClick={onOpen}
    >
      <svg className="pulse-ring" viewBox={`0 0 ${VB} ${VB}`} aria-hidden="true">
        <g transform={`rotate(150 ${VB / 2} ${VB / 2})`}>
          <circle className="pulse-track" cx={VB / 2} cy={VB / 2} r={R} strokeDasharray={`${FULL} ${C}`} />
          {prog > 0 && (
            <circle className="pulse-arc" cx={VB / 2} cy={VB / 2} r={R} strokeDasharray={`${prog} ${C}`} />
          )}
        </g>
      </svg>
      <span className="pulse-text">{text}</span>
      <span className="pulse-bar" aria-hidden="true">
        <span className="pulse-fill" style={{ width: `${Math.max(0, Math.min(1, frac)) * 100}%` }} />
      </span>
    </button>
  )
}
