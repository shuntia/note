import { useCallback, useEffect, useState } from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { DayLine, minutesOf } from '../dayline'
import { Overflow } from '../overflow'
import { eventLabel } from '../receipts'
import type { Debrief, PlanEvent } from '../types'

const UNDO_MS = 5000
const LATER_MINUTES = [5, 10, 15, 30, 60]
const FOLD_KEY = 'note.debriefFolded'

// Drop has no server-side reversal, so the request waits out the undo window before it
// is sent. Module scope keeps the hold alive across the remounts a websocket nudge causes.
let heldDrop: { id: number; timer: number } | null = null

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

// The fired event owns the hero; failing that, the next routine still ahead does.
function nextUp(events: PlanEvent[], now: number): PlanEvent | null {
  return (
    events.find((ev) => ev.status === 'fired') ??
    events.find(
      (ev) =>
        ev.entry !== 'block' &&
        (ev.status === 'pending' || ev.status === 'snoozed') &&
        minutesOf(ev.end_wall_time ?? ev.wall_time) >= now,
    ) ??
    null
  )
}

export function Today({ notify, openNow, onChanged }: ViewProps) {
  const [events, setEvents] = useState<PlanEvent[] | null>(null)
  const [pending, setPending] = useState(false)
  const [later, setLater] = useState(false)
  const [, tick] = useState(0)

  const load = useCallback(() => {
    api.planToday().then(setEvents).catch(() => setEvents([]))
  }, [])
  useEffect(load, [load])

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

  const commitDrop = useCallback(() => {
    if (!heldDrop) return
    const { id, timer } = heldDrop
    heldDrop = null
    window.clearTimeout(timer)
    api.eventAction(id, 'drop').then(load).catch(() => load())
  }, [load])
  useEffect(() => commitDrop, [commitDrop])

  const drop = (ev: PlanEvent) => {
    commitDrop()
    heldDrop = { id: ev.id, timer: window.setTimeout(commitDrop, UNDO_MS) }
    tick((n) => n + 1)
    notify(`Dropped ${eventLabel(ev.kind)}`, {
      label: 'Undo',
      run: () => {
        if (heldDrop?.id !== ev.id) return
        window.clearTimeout(heldDrop.timer)
        heldDrop = null
        tick((n) => n + 1)
      },
    })
  }

  const visible = events?.filter((ev) => ev.id !== heldDrop?.id) ?? []
  const now = nowMinutes()
  const next = nextUp(visible, now)
  const nowLabel = `${String(Math.floor(now / 60)).padStart(2, '0')}:${String(now % 60).padStart(2, '0')}`

  return (
    <div className="today">
      <section className="today-hero">
        {next ? (
          <>
            <div className="today-eyebrow">
              NOW {nowLabel} <span className="today-dot" aria-hidden="true" /> {next.status === 'fired' ? 'NOW' : 'UP NEXT'}
            </div>
            <h1 className="today-title">{eventLabel(next.kind)}</h1>
            <div className="today-when">
              <span className="today-in">in {Math.max(0, minutesOf(next.wall_time) - now)} min</span>
              <span className="today-span">{next.wall_time} – {next.end_wall_time ?? next.wall_time}</span>
            </div>
            <div className="today-actions">
              <button
                className="btn-fill"
                disabled={pending}
                onClick={() =>
                  openNow({
                    taskId: null, eventId: next.id, title: eventLabel(next.kind), notes: '',
                    stepIndex: null, stepCount: null, stepName: null,
                    durationSec: Math.max(60, (minutesOf(next.end_wall_time ?? next.wall_time) - minutesOf(next.wall_time)) * 60),
                    startedAt: Date.now(), pausedAt: null, pausedMs: 0,
                  })
                }
              >
                Start
              </button>
              <button className="btn-haze" aria-expanded={later} disabled={pending} onClick={() => setLater((v) => !v)}>
                Later
              </button>
              <Overflow
                label="More"
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
      <section className="today-line">
        <DayLine events={visible} now={now} />
      </section>
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

function DebriefFold() {
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
    <section className="debrief-row">
      <button className="debrief-fold" aria-expanded={!folded} onClick={toggle}>
        <span className="debrief-mark" aria-hidden="true" />
        <span className="debrief-lead">
          <b>This morning:</b> {folded ? firstSentence(debrief.content) : ''}
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
