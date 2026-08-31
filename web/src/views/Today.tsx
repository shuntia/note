import { useCallback, useEffect, useState, type ReactNode } from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { SectionTitle } from '../section'
import type { Debrief, PlanEvent } from '../types'

function nowWall(): string {
  const d = new Date()
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
}

function label(kind: string): string {
  if (kind.includes('checkin')) return 'Check-in'
  if (kind === 'debrief') return 'Morning debrief'
  if (kind === 'nudge') return 'Nudge'
  return kind.replaceAll('_', ' ')
}

const STATUS_WORD: Record<PlanEvent['status'], string> = {
  pending: '',
  fired: 'waiting on you',
  snoozed: 'later',
  done: 'done',
  dropped: 'dropped',
}

function actionMessage(err: unknown): string {
  if (err instanceof ApiError) {
    if (err.status === 400) return "That's outside this event's slide window."
    if (err.status === 409) return 'Already settled — refresh to see its state.'
    if (err.status === 404) return 'That event is gone.'
  }
  return 'Something went wrong. Try again.'
}

export function Today({ notify }: ViewProps) {
  const [events, setEvents] = useState<PlanEvent[] | null>(null)
  const [failed, setFailed] = useState(false)
  const [pending, setPending] = useState(false)
  const [, tick] = useState(0)

  const load = useCallback(() => {
    api
      .planToday()
      .then((evs) => {
        setEvents(evs)
        setFailed(false)
      })
      .catch(() => setFailed(true))
  }, [])

  useEffect(() => {
    load()
  }, [load])

  useEffect(() => {
    const id = window.setInterval(() => tick((n) => n + 1), 60_000)
    return () => window.clearInterval(id)
  }, [])

  // Event routes are relative operations, so a second tap before the first lands compounds it.
  const act = async (fn: () => Promise<void>) => {
    if (pending) return
    setPending(true)
    try {
      await fn()
      load()
    } catch (err) {
      notify(actionMessage(err))
      if (err instanceof ApiError && err.status === 409) load()
    } finally {
      setPending(false)
    }
  }

  return (
    <div className="page today">
      <section className="today-plan">
        <SectionTitle>Plan</SectionTitle>
        {failed ? (
          <p className="muted">
            Couldn't load today's plan.{' '}
            <button className="quiet" onClick={load}>
              Retry
            </button>
          </p>
        ) : events === null ? null : events.length === 0 ? (
          <p className="muted">Nothing planned today.</p>
        ) : (
          <ul className="spine">{spine(events, act, pending)}</ul>
        )}
      </section>
      <aside className="today-aside">
        <DebriefCard />
      </aside>
    </div>
  )
}

function spine(
  events: PlanEvent[],
  act: (fn: () => Promise<void>) => Promise<void>,
  pending: boolean,
): ReactNode[] {
  const now = nowWall()
  const rows: ReactNode[] = []
  let markerPlaced = false
  for (const ev of events) {
    if (!markerPlaced && ev.wall_time > now) {
      rows.push(<NowMarker key="now" now={now} />)
      markerPlaced = true
    }
    rows.push(
      <EventRow key={ev.id} ev={ev} past={ev.wall_time <= now} act={act} pending={pending} />,
    )
  }
  if (!markerPlaced) rows.push(<NowMarker key="now" now={now} />)
  return rows
}

function NowMarker({ now }: { now: string }) {
  return (
    <li className="now-marker" aria-label={`current time ${now}`}>
      <span className="time">{now}</span>
      <span className="rule" />
    </li>
  )
}

function EventRow({
  ev,
  past,
  act,
  pending,
}: {
  ev: PlanEvent
  past: boolean
  act: (fn: () => Promise<void>) => Promise<void>
  pending: boolean
}) {
  const settled = ev.status === 'done' || ev.status === 'dropped'
  return (
    <li className={past ? 'past' : ''}>
      <span className="time">{ev.wall_time}</span>
      <div className="card event-card">
        <div className="event-head">
          <span className="event-kind">{label(ev.kind)}</span>
          {ev.status !== 'pending' && (
            <span className={`event-status ${ev.status}`}>{STATUS_WORD[ev.status]}</span>
          )}
        </div>
        {!settled && (
          <div className="event-actions">
            <button
              className="ghost"
              disabled={pending}
              onClick={() => act(() => api.eventAction(ev.id, 'done'))}
            >
              Done
            </button>
            <button
              className="ghost"
              disabled={pending}
              onClick={() => act(() => api.snooze(ev.id, 30))}
            >
              Later
            </button>
            {ev.flexibility !== 'fixed' && (
              <>
                <button
                  className="ghost"
                  disabled={pending}
                  onClick={() => act(() => api.shift(ev.id, 15))}
                >
                  +15
                </button>
                <button
                  className="ghost"
                  disabled={pending}
                  onClick={() => act(() => api.shift(ev.id, -15))}
                >
                  −15
                </button>
              </>
            )}
            <button
              className="ghost danger"
              disabled={pending}
              onClick={() => act(() => api.eventAction(ev.id, 'drop'))}
            >
              Drop
            </button>
          </div>
        )}
      </div>
    </li>
  )
}

function DebriefCard() {
  const [debrief, setDebrief] = useState<Debrief | null | 'error' | undefined>(undefined)

  const load = () => {
    setDebrief(undefined)
    api
      .debrief()
      .then(setDebrief)
      .catch((err) => setDebrief(err instanceof ApiError && err.status === 404 ? null : 'error'))
  }
  useEffect(load, [])

  const date = debrief && debrief !== 'error' ? debrief.date : undefined
  return (
    <section className="card debrief">
      <SectionTitle meta={date}>Debrief</SectionTitle>
      {debrief === undefined ? (
        <p className="muted">Loading…</p>
      ) : debrief === null ? (
        <p className="muted">No debrief yet — it arrives overnight.</p>
      ) : debrief === 'error' ? (
        <p className="muted">
          The debrief didn't load.{' '}
          <button className="quiet" onClick={load}>
            Retry
          </button>
        </p>
      ) : (
        <div className="letter">{debrief.content}</div>
      )}
    </section>
  )
}
