import { useCallback, useEffect, useState, type ReactNode } from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { Overflow } from '../overflow'
import { eventLabel } from '../receipts'
import type { Debrief, PlanEvent } from '../types'

const UNDO_MS = 5000
const FOLD_KEY = 'note.debriefFolded'

// Drop has no server-side reversal, so the request waits out the undo window before it
// is sent. Module scope keeps the hold alive across the remounts a websocket nudge causes.
let heldDrop: { id: number; timer: number } | null = null

function nowWall(): string {
  const d = new Date()
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
}

function minutesOf(wall: string): number {
  const [h, m] = wall.split(':')
  return Number(h) * 60 + Number(m)
}

function eyebrow(ev: PlanEvent, now: string): string {
  if (ev.status === 'fired') return 'NOW'
  const mins = minutesOf(ev.wall_time) - minutesOf(now)
  if (mins <= 0) return 'UP NEXT'
  if (mins < 90) return `UP NEXT · IN ${mins} MIN`
  return `UP NEXT · IN ${Math.round(mins / 60)} HR`
}

function slideText(ev: PlanEvent): string {
  if (ev.flexibility === 'fixed') return 'Happens at a fixed time'
  if (ev.flexibility === 'drop') return 'Can be dropped if the day fills up'
  return ev.slide_window_min > 0 ? `Can slide ±${ev.slide_window_min} min` : 'Can slide freely'
}

function reachText(channel: string): string {
  if (channel === 'push') return 'reaches you as a push'
  if (channel === 'voice') return 'reaches you as a call'
  return ''
}

function metaLine(ev: PlanEvent): string {
  return [slideText(ev), reachText(ev.channel)].filter(Boolean).join(' · ')
}

function flexTag(ev: PlanEvent): string {
  if (ev.status === 'snoozed') return 'later'
  if (ev.flexibility === 'fixed') return 'fixed'
  if (ev.flexibility === 'drop') return 'droppable'
  return ev.slide_window_min > 0 ? `±${ev.slide_window_min} min` : 'flexible'
}

function actionMessage(err: unknown): string {
  if (err instanceof ApiError) {
    if (err.status === 400) return "That's outside this event's slide window."
    if (err.status === 409) return 'Already settled — refresh to see its state.'
    if (err.status === 404) return 'That event is gone.'
  }
  return 'Something went wrong. Try again.'
}

// The fired event owns Now; failing that, the next one still open does.
function currentIndex(events: PlanEvent[]): number {
  const fired = events.findIndex((ev) => ev.status === 'fired')
  if (fired !== -1) return fired
  return events.findIndex((ev) => ev.status === 'pending' || ev.status === 'snoozed')
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

  // Landing on the minute boundary keeps the Now label and the card's countdown honest.
  useEffect(() => {
    let timer = 0
    const schedule = () => {
      timer = window.setTimeout(
        () => {
          tick((n) => n + 1)
          schedule()
        },
        60_000 - (Date.now() % 60_000) + 50,
      )
    }
    schedule()
    return () => window.clearTimeout(timer)
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

  const commitDrop = useCallback(() => {
    if (!heldDrop) return
    const { id, timer } = heldDrop
    heldDrop = null
    window.clearTimeout(timer)
    api
      .eventAction(id, 'drop')
      .then(load)
      .catch(() => load())
  }, [load])

  useEffect(() => commitDrop, [commitDrop])

  const drop = (ev: PlanEvent) => {
    commitDrop()
    heldDrop = { id: ev.id, timer: window.setTimeout(commitDrop, UNDO_MS) }
    tick((n) => n + 1)
    notify(`Dropped "${eventLabel(ev.kind)}" — moved off today`, {
      label: 'Undo',
      run: () => {
        if (heldDrop?.id !== ev.id) return
        window.clearTimeout(heldDrop.timer)
        heldDrop = null
        tick((n) => n + 1)
      },
    })
  }

  const visible = events?.filter((ev) => ev.id !== heldDrop?.id) ?? null

  return (
    <div className="page today">
      <DebriefFold />
      {failed ? (
        <p className="muted">
          Couldn't load today's plan.{' '}
          <button className="quiet" onClick={load}>
            Retry
          </button>
        </p>
      ) : visible === null ? null : visible.length === 0 ? (
        <p className="muted">Nothing planned today.</p>
      ) : (
        <>
          <ul className="spine">{spine(visible, act, drop, pending)}</ul>
          <p className="today-tomorrow">
            Tomorrow's plan arrives overnight — nothing for you to set up.
          </p>
        </>
      )}
    </div>
  )
}

function spine(
  events: PlanEvent[],
  act: (fn: () => Promise<void>) => Promise<void>,
  drop: (ev: PlanEvent) => void,
  pending: boolean,
): ReactNode[] {
  const now = nowWall()
  const current = currentIndex(events)
  const rows: ReactNode[] = []
  events.forEach((ev, i) => {
    if (i === current) {
      rows.push(<NowLine key="now" now={now} />)
      rows.push(<NowCard key={ev.id} ev={ev} now={now} act={act} drop={drop} pending={pending} />)
      return
    }
    rows.push(<EventRow key={ev.id} ev={ev} />)
  })
  if (current === -1) {
    rows.push(<NowLine key="now" now={now} />)
    rows.push(
      <li key="clear" className="today-clear">
        That's everything today.
      </li>,
    )
  }
  return rows
}

function NowLine({ now }: { now: string }) {
  return (
    <li className="now-line">
      <span className="now-rule" aria-hidden="true" />
      <span className="now-dot" aria-hidden="true" />
      <span className="now-label">NOW · {now}</span>
    </li>
  )
}

function EventRow({ ev }: { ev: PlanEvent }) {
  const state = ev.status === 'done' ? 'done' : ev.status === 'dropped' ? 'dropped' : ''
  const tag = state === 'dropped' ? 'dropped' : state === 'done' ? '' : flexTag(ev)
  return (
    <li className={`ev ${state}`}>
      <span className="ev-time">{ev.wall_time}</span>
      <span className="ev-dot" aria-hidden="true" />
      <div className="ev-row">
        {state === 'done' && (
          <span className="ev-check" aria-hidden="true">
            ✓
          </span>
        )}
        <span className="ev-name">{eventLabel(ev.kind)}</span>
        <span className="ev-tag">{tag}</span>
      </div>
    </li>
  )
}

function NowCard({
  ev,
  now,
  act,
  drop,
  pending,
}: {
  ev: PlanEvent
  now: string
  act: (fn: () => Promise<void>) => Promise<void>
  drop: (ev: PlanEvent) => void
  pending: boolean
}) {
  return (
    <li className="ev now">
      <span className="ev-time">{ev.wall_time}</span>
      <div className="nowcard">
        <Overflow
          label="More actions"
          items={[{ label: 'Drop', run: () => drop(ev), disabled: pending }]}
        />
        <div className="nowcard-eyebrow">{eyebrow(ev, now)}</div>
        <h2 className="nowcard-title">{eventLabel(ev.kind)}</h2>
        <p className="nowcard-meta">{metaLine(ev)}</p>
        <div className="nowcard-actions">
          <button
            className="btn-primary"
            disabled={pending}
            onClick={() => act(() => api.eventAction(ev.id, 'done'))}
          >
            Done
          </button>
          <button
            className="btn-outline"
            disabled={pending}
            onClick={() => act(() => api.snooze(ev.id, 30))}
          >
            Later
          </button>
          <span className="actions-spacer" />
          {ev.flexibility !== 'fixed' && (
            <>
              <button
                className="btn-chip"
                disabled={pending}
                onClick={() => act(() => api.shift(ev.id, -15))}
              >
                −15
              </button>
              <button
                className="btn-chip"
                disabled={pending}
                onClick={() => act(() => api.shift(ev.id, 15))}
              >
                +15
              </button>
            </>
          )}
        </div>
      </div>
    </li>
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
        <span aria-hidden="true">☀︎</span>
        <span className="debrief-lead">
          <b>This morning:</b> {folded ? firstSentence(debrief.content) : ''}
        </span>
        <span className="debrief-chev" aria-hidden="true">
          {folded ? '▾' : '▴'}
        </span>
      </button>
      {!folded && <div className="letter">{debrief.content}</div>}
    </section>
  )
}
