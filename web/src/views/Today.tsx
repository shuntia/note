import { useCallback, useEffect, useState } from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { DayLine, minutesOf } from '../dayline'
import { useEscape } from '../escape'
import { eventFacts, nextUp } from '../events'
import { makeHold } from '../held'
import { Overflow } from '../overflow'
import { eventLabel } from '../receipts'
import type { Debrief, PlanEvent } from '../types'

const LATER_MINUTES = [5, 10, 15, 30, 60]
const ROUTINE_MIN = 15
const FOLD_KEY = 'note.debriefFolded'

// Drop has no server-side reversal, so the request waits out the undo window.
const dropHold = makeHold<number>()

function nowMinutes(): number {
  const d = new Date()
  return d.getHours() * 60 + d.getMinutes()
}

function actionMessage(err: unknown): string {
  if (err instanceof ApiError) {
    if (err.status === 409) return 'Already settled.'
    if (err.status === 404) return 'That event is gone.'
  }
  return 'Something went wrong. Try again.'
}

export function Today({ notify, openNow, onChanged, refresh }: ViewProps) {
  const [events, setEvents] = useState<PlanEvent[] | null>(null)
  const [pending, setPending] = useState(false)
  const [later, setLater] = useState(false)
  useEscape(later, () => setLater(false))
  const [, tick] = useState(0)

  const load = useCallback(() => {
    api
      .planToday()
      .then(setEvents)
      .catch(() => notify("Couldn't load today. Try again."))
  }, [notify])
  useEffect(load, [load, refresh])

  useEffect(() => {
    const id = setInterval(() => tick((n) => n + 1), 30_000)
    return () => clearInterval(id)
  }, [])

  const act = async (fn: () => Promise<unknown>) => {
    if (pending) return
    setPending(true)
    try {
      await fn()
      load()
      onChanged()
    } catch (err) {
      notify(actionMessage(err))
      if (err instanceof ApiError && err.status === 409) load()
    } finally {
      setPending(false)
    }
  }

  const drop = (ev: PlanEvent) => {
    // The hold outlives this component, so the commit also pokes the app-level
    // refresh that a remounted view is listening to.
    const settled = () => {
      load()
      onChanged()
    }
    dropHold.start(ev.id, () => {
      api.eventAction(ev.id, 'drop').then(settled, settled)
    })
    tick((n) => n + 1)
    notify(`Dropped ${eventLabel(ev.kind)}`, {
      label: 'Undo',
      run: () => {
        if (dropHold.cancel(ev.id)) tick((n) => n + 1)
      },
    })
  }

  // A routine is timed to its span; without an end the routine default stands in.
  const start = (ev: PlanEvent) => {
    const span = ev.end_wall_time
      ? Math.max(1, minutesOf(ev.end_wall_time) - minutesOf(ev.wall_time))
      : ROUTINE_MIN
    openNow({
      taskId: null,
      eventId: ev.id,
      title: eventLabel(ev.kind),
      notes: '',
      stepIndex: null,
      stepCount: null,
      stepName: null,
      durationSec: span * 60,
      startedAt: Date.now(),
      pausedAt: null,
      pausedMs: 0,
    })
  }

  const visible = events?.filter((ev) => ev.id !== dropHold.held()) ?? []
  const now = nowMinutes()
  const next = nextUp(visible, now)
  const facts = next ? eventFacts(next, now) : null
  const nowLabel = `${String(Math.floor(now / 60)).padStart(2, '0')}:${String(now % 60).padStart(2, '0')}`

  return (
    <div className="today">
      <section className="today-hero">
        {next && facts ? (
          <>
            <div className="today-eyebrow">
              NOW {nowLabel}
              {facts.eyebrow === 'NEXT' && (
                <>
                  {' '}
                  <span className="today-dot" aria-hidden="true" /> UP NEXT
                </>
              )}
            </div>
            <h1 className="today-title">{eventLabel(next.kind)}</h1>
            <div className="today-when">
              {facts.minutes !== null && <span className="today-in">in {facts.minutes} min</span>}
              <span className="today-span">{facts.span}</span>
            </div>
            <div className="today-actions">
              <button className="btn-fill" disabled={pending} onClick={() => start(next)}>
                Start
              </button>
              <button className="btn-haze" aria-expanded={later} disabled={pending} onClick={() => setLater((v) => !v)}>
                Later
              </button>
              <Overflow
                label="More"
                className={`ev-more-wrap${later ? ' beside-later' : ''}`}
                items={[
                  { label: 'Drop today', run: () => drop(next), disabled: pending },
                  { label: 'Move to tomorrow', run: () => act(() => api.moveTomorrow(next.id)), disabled: pending },
                  { label: next.alert ? 'Silent' : 'Ping me', run: () => act(() => api.setEventAlert(next.id, !next.alert)), disabled: pending },
                ]}
              />
              {later && (
                <div className="later-pick" role="group" aria-label="Later by">
                  <span className="later-lead">Later by</span>
                  {LATER_MINUTES.map((m) => (
                    <button key={m} className="later-min" disabled={pending} onClick={() => { setLater(false); act(() => api.snooze(next.id, m)) }}>
                      {m}
                    </button>
                  ))}
                  <span className="later-unit">min</span>
                </div>
              )}
            </div>
          </>
        ) : (
          events && <h1 className="today-title">That's everything today.</h1>
        )}
      </section>
      {events && (
        <section className="today-line">
          <DayLine events={visible} now={now} nextId={next?.id} />
        </section>
      )}
      <section className="today-ground">
        <DebriefFold />
      </section>
    </div>
  )
}

function readFold(date: string): boolean {
  try {
    const raw = localStorage.getItem(FOLD_KEY)
    if (!raw) return true
    const saved = JSON.parse(raw) as { date?: string; folded?: boolean }
    return saved.date === date ? saved.folded !== false : true
  } catch {
    return true
  }
}

function writeFold(date: string, folded: boolean) {
  try {
    localStorage.setItem(FOLD_KEY, JSON.stringify({ date, folded }))
  } catch {
    // storage blocked; the fold still holds for this session
  }
}

// The trailing full stop gives way to the ellipsis rather than stacking with it.
function firstSentence(text: string): string {
  const trimmed = text.trim()
  const match = /^[\s\S]*?[.!?](?=\s|$)/.exec(trimmed)
  const lead = match ? match[0] : trimmed
  if (lead.length === trimmed.length) return lead
  return `${lead.replace(/\.$/, '')}…`
}

export function DebriefFold() {
  const [debrief, setDebrief] = useState<Debrief | null | 'error' | undefined>(undefined)
  const [folded, setFolded] = useState(true)

  const load = () => {
    setDebrief(undefined)
    api
      .debrief()
      .then((d) => {
        setDebrief(d)
        setFolded(readFold(d.date))
      })
      .catch((err) => setDebrief(err instanceof ApiError && err.status === 404 ? null : 'error'))
  }
  useEffect(load, [])

  if (debrief === undefined) return null
  if (debrief === null) {
    return <p className="debrief-note muted">No letter yet — it arrives overnight.</p>
  }
  if (debrief === 'error') {
    return (
      <p className="debrief-note muted">
        The morning letter didn't load.{' '}
        <button className="quiet" onClick={load}>
          Retry
        </button>
      </p>
    )
  }

  const toggle = () => {
    const next = !folded
    setFolded(next)
    writeFold(debrief.date, next)
  }

  return (
    <section className={`debrief-row${folded ? '' : ' open'}`}>
      <button className="debrief-fold" aria-expanded={!folded} onClick={toggle}>
        <span className="debrief-mark" aria-hidden="true" />
        <span className="debrief-lead">
          <b>This morning:</b> {firstSentence(debrief.content)}
        </span>
        <span className="debrief-chev" aria-hidden="true">
          <svg viewBox="0 0 24 24">
            <path d="M6 9l6 6 6-6" />
          </svg>
        </span>
      </button>
      {!folded && <div className="letter">{debrief.content}</div>}
    </section>
  )
}
