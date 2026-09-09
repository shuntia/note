import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react'
import { api, ApiError } from '../api'
import type { ToastAction } from '../app'
import { DayLine, minutesOf } from '../dayline'
import { eventFacts, nextUp } from '../events'
import { Gauge } from '../gauge'
import { makeHold } from '../held'
import { NowCounter } from '../nowcounter'
import { Overflow } from '../overflow'
import { Presence } from '../presence'
import { readPrefs } from '../prefs'
import { eventLabel } from '../receipts'
import { effectiveStart, elapsedSec, type FocusSession } from '../session'
import { useStage } from '../stage'
import { TellNote } from '../tellnote'
import type { PlanEvent } from '../types'

const LATER_MINUTES = [5, 10, 15, 30, 60]
const ROUTINE_MIN = 15

// Drop has no server-side reversal, so the request waits out the undo window.
const dropHold = makeHold<number>()
// Nor does finishing, so the last step's write waits the same way.
const doneHold = makeHold<FocusSession>()

const round5 = (min: number) => Math.max(5, Math.round(min / 5) * 5)

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

// Where the wait started: the end of the last settled routine before now, else 06:00.
function waitStart(events: PlanEvent[], now: number): number {
  const ended = events
    .filter((ev) => ev.status === 'done' || ev.status === 'dropped')
    .map((ev) => minutesOf(ev.end_wall_time ?? ev.wall_time))
    .filter((m) => m <= now)
  return ended.length ? Math.max(...ended) : 6 * 60
}

// The column is replaced rather than appended to server-side, so the text the
// session started with has to travel back out with the new line.
function withElapsedNote(previous: string, elapsed: number): string {
  const day = new Date().toISOString().slice(0, 10)
  const line = `${day} · focused ${Math.max(1, Math.round(elapsed / 60))} min`
  return previous.trim() ? `${previous.trim()}\n${line}` : line
}

export function Home({
  session,
  setSession,
  notify,
  onChanged,
  refresh,
  openNow,
  mobile,
  onChrome,
  tabs,
}: {
  session: FocusSession | null
  setSession: (s: FocusSession | null) => void
  notify: (msg: string, action?: ToastAction) => void
  onChanged: () => void
  refresh: number
  openNow: (s: FocusSession) => void
  mobile: boolean
  onChrome: (hidden: boolean) => void
  tabs: ReactNode
}) {
  const [events, setEvents] = useState<PlanEvent[]>([])
  const [beat, tick] = useState(0)
  const [pending, setPending] = useState(false)
  const [later, setLater] = useState(false)
  const inSession = session !== null
  const { stage, setStage, bind } = useStage(inSession ? 2 : 1)
  const [wasInSession, setWasInSession] = useState(inSession)
  const prefs = readPrefs()

  // Starting or finishing puts the face back at stage 0 in the same render, so no
  // frame shows the new face at the old stage.
  if (wasInSession !== inSession) {
    setWasInSession(inSession)
    setStage(0)
  }

  const load = useCallback(() => {
    api
      .planToday()
      .then(setEvents)
      .catch(() => setEvents([]))
  }, [])
  useEffect(load, [load, refresh])

  useEffect(() => {
    const id = setInterval(() => tick((n) => n + 1), 1000)
    return () => clearInterval(id)
  }, [])

  // The last stage is Today; before it the face owns the whole screen.
  const showToday = stage === (inSession ? 2 : 1)
  useEffect(() => {
    onChrome(!showToday)
    return () => onChrome(false)
  }, [showToday, onChrome])

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

  // the beat is a dependency because the holds live outside React state
  const visible = useMemo(
    () => events.filter((ev) => ev.id !== dropHold.held() && ev.id !== doneHold.held()?.eventId),
    [events, beat],
  )
  const now = nowMinutes()
  const next = nextUp(visible, now)
  const facts = next ? eventFacts(next, now) : null

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

  // ── session face ─────────────────────────────────────────────
  const pause = () => session && setSession({ ...session, pausedAt: Date.now() })
  const resume = () =>
    session &&
    setSession({
      ...session,
      pausedAt: null,
      pausedMs: session.pausedMs + (Date.now() - (session.pausedAt ?? Date.now())),
    })
  // Ending the last step closes the session at once and holds the write, so Undo
  // is a toast rather than a question asked before the fact.
  const complete = (s: FocusSession) => {
    const elapsed = elapsedSec(s)
    const send = () => {
      if (s.taskId !== null) {
        api
          .patchTask(s.taskId, { state: 'done', notes: withElapsedNote(s.notes, elapsed) })
          .then(onChanged)
          .catch(() => notify("Couldn't save the session. Try again."))
      } else if (s.eventId !== null) {
        api
          .eventAction(s.eventId, 'done')
          .then(onChanged)
          .catch(() => notify("Couldn't mark that done. Try again."))
      }
    }
    doneHold.start(s, send)
    setSession(null)
    onChanged()
    notify('Done', {
      label: 'Undo',
      run: () => {
        if (!doneHold.cancel(s)) return
        tick((n) => n + 1)
        setSession(s)
      },
    })
  }

  // A step before the last hands the session straight to the next one still open.
  const advance = async (s: FocusSession, step: number) => {
    if (pending) return
    setPending(true)
    try {
      await api.patchTask(step, { state: 'done', notes: withElapsedNote(s.notes, elapsedSec(s)) })
      const nodes = await api.tasks()
      const parent = nodes.find((n) => n.children.some((c) => c.id === step))
      const at = parent?.children.findIndex((c) => c.id === step) ?? -1
      const open = parent?.children.slice(at + 1).find((c) => c.state === 'open' || c.state === 'in_progress')
      onChanged()
      if (!parent || !open) {
        setSession(null)
        notify('Done')
        return
      }
      setSession({
        taskId: open.id,
        eventId: null,
        title: parent.title,
        notes: open.notes,
        stepIndex: parent.children.indexOf(open) + 1,
        stepCount: parent.children.length,
        stepName: open.title,
        durationSec: open.duration_min === null ? null : round5(open.duration_min) * 60,
        startedAt: Date.now(),
        pausedAt: null,
        pausedMs: 0,
      })
    } catch {
      notify("Couldn't save the session. Try again.")
    } finally {
      setPending(false)
    }
  }

  const finish = () => {
    if (!session) return
    const more = session.stepIndex !== null && session.stepCount !== null && session.stepIndex < session.stepCount
    if (session.taskId !== null && more) advance(session, session.taskId)
    else complete(session)
  }

  // Past the duration the counter leaves the preference behind and counts the overrun up.
  const overrun = (s: FocusSession) => {
    const total = s.durationSec
    return { total, over: total !== null && elapsedSec(s) > total }
  }

  const counter = (s: FocusSession) => {
    const { total, over } = overrun(s)
    return (
      <NowCounter
        startedAt={effectiveStart(s) + (over ? (total ?? 0) * 1000 : 0)}
        durationSec={total ?? 0}
        mode={over || total === null ? 'elapsed' : prefs.counter}
        pausedAt={s.pausedAt}
      />
    )
  }

  const sessionFrac = (session: FocusSession) =>
    session.durationSec ? elapsedSec(session) / session.durationSec : 0

  // ── wait face ────────────────────────────────────────────────
  const waitCentre = (ev: PlanEvent, big: boolean) => {
    const { eyebrow, minutes, span } = eventFacts(ev, now)
    return (
      <>
        {big && <div className="gauge-eyebrow">{eyebrow}</div>}
        {minutes !== null && (
          <div className="gauge-num" style={{ fontSize: big ? 50 : 22 }}>{minutes} min</div>
        )}
        {big && (
          <>
            <div className="gauge-name" style={{ fontSize: 18 }}>{eventLabel(ev.kind)}</div>
            <div className="gauge-sub">{span}</div>
          </>
        )}
      </>
    )
  }
  const waitFrac = (ev: PlanEvent) => {
    if (eventFacts(ev, now).minutes === null) return 1
    const from = waitStart(visible, now)
    const to = minutesOf(ev.wall_time)
    return to <= from ? 1 : (now - from) / (to - from)
  }

  const nextActions = (ev: PlanEvent) => (
    <>
      <button className="btn-fill" disabled={pending} onClick={() => start(ev)}>Start</button>
      <button className="btn-haze" aria-expanded={later} disabled={pending} onClick={() => setLater((v) => !v)}>Later</button>
      <Overflow
        label="More"
        items={[
          { label: 'Drop today', run: () => drop(ev), disabled: pending },
          { label: 'Move to tomorrow', run: () => act(() => api.moveTomorrow(ev.id)), disabled: pending },
          { label: ev.alert ? 'Silent' : 'Ping me', run: () => act(() => api.setEventAlert(ev.id, !ev.alert)), disabled: pending },
        ]}
      />
      {later && (
        <div className="later-pick" role="group" aria-label="Later by">
          <span className="later-lead">Later by</span>
          {LATER_MINUTES.map((m) => (
            <button key={m} className="later-min" disabled={pending} onClick={() => { setLater(false); act(() => api.snooze(ev.id, m)) }}>
              {m}
            </button>
          ))}
          <span className="later-unit">min</span>
        </div>
      )}
    </>
  )

  const chevron = (
    <button className="chev" aria-label="Today" onClick={() => setStage((s) => Math.min(inSession ? 2 : 1, s + 1))}>
      <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 14l6-6 6 6" /></svg>
    </button>
  )

  const today = (
    <>
      <DayLine events={visible} now={now} compact={mobile} nextId={next?.id} />
      <ul className="home-list">
        {visible
          .filter((ev) => (ev.status === 'pending' || ev.status === 'snoozed' || ev.status === 'fired') && minutesOf(ev.end_wall_time ?? ev.wall_time) >= now)
          .map((ev) => (
            <li key={ev.id} className={ev.id === next?.id ? 'next' : ''}>
              <span className="home-when">{ev.wall_time} – {ev.end_wall_time ?? ev.wall_time}</span>
              <span className="home-what">{eventLabel(ev.kind)}</span>
            </li>
          ))}
      </ul>
      {mobile && <TellNote notify={notify} />}
    </>
  )

  // ── layout ───────────────────────────────────────────────────
  if (session) {
    const big = stage === 0
    const { over } = overrun(session)
    return (
      <div className={`home in-session stage-${stage}${mobile ? ' mobile' : ''}${session.pausedAt ? ' paused' : ''}`} {...bind}>
        <div className="home-face">
          <Gauge size={stage === 2 ? 120 : big ? (mobile ? 320 : 440) : 230} frac={sessionFrac(session)}>
            <div className={`gauge-num${over ? ' over' : ''}`} style={{ fontSize: stage === 2 ? 24 : big ? 58 : 40 }}>
              {over ? '+' : ''}
              {counter(session)}
            </div>
            {stage !== 2 && (
              <>
                <div className="gauge-name" style={{ fontSize: big ? 18 : 15 }}>{session.stepName ?? session.title}</div>
                {session.stepIndex !== null && (
                  <div className="gauge-sub">{session.stepIndex} of {session.stepCount}</div>
                )}
              </>
            )}
          </Gauge>
          <Presence key="head" show={stage === 2} className="home-head">
            <span className="home-head-name">{session.stepName ?? session.title}</span>
            {session.stepIndex !== null && <span className="gauge-sub">{session.stepIndex} of {session.stepCount}</span>}
          </Presence>
        </div>
        <Presence key="sheet" show={stage === 1} className="home-sheet">
          <button className="btn-round" aria-label={session.pausedAt ? 'Back to it' : 'Break'} onClick={session.pausedAt ? resume : pause}>
              {session.pausedAt ? (
                <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 5.5v13l10-6.5z" /></svg>
              ) : (
                <svg viewBox="0 0 24 24" aria-hidden="true"><rect x="6" y="5" width="4" height="14" rx="1.2" /><rect x="14" y="5" width="4" height="14" rx="1.2" /></svg>
              )}
            </button>
          <button className="btn-fill wide" disabled={pending} onClick={finish}>Done with this step</button>
          <TellNote notify={notify} />
        </Presence>
        <Presence key="today" show={stage === 2} className="home-today">{today}</Presence>
        {stage === 2 && mobile && tabs}
        {stage < 2 && chevron}
      </div>
    )
  }

  return (
    <div className={`home stage-${stage}${mobile ? ' mobile' : ''}`} {...bind}>
      <div className="home-face">
        {next ? (
          prefs.showArc ? (
            <Gauge size={stage === 1 ? 120 : mobile ? 320 : 440} frac={waitFrac(next)} faded>
              {waitCentre(next, stage === 0)}
            </Gauge>
          ) : (
            stage === 0 && facts && (
              <div className="home-text">
                <div className="gauge-eyebrow">{facts.eyebrow}</div>
                <div className="home-title">{eventLabel(next.kind)}</div>
                {facts.minutes !== null && (
                  <div className="gauge-num" style={{ fontSize: 30 }}>in {facts.minutes} min</div>
                )}
                <div className="gauge-sub">{facts.span}</div>
              </div>
            )
          )
        ) : (
          <div className="home-text"><div className="home-title">That's everything today.</div></div>
        )}
        <Presence key="head" show={stage === 1 && next !== null && facts !== null} className="home-head">
          <span className="gauge-eyebrow">{facts?.eyebrow}</span>
          <span className="home-head-name">{next && eventLabel(next.kind)}</span>
          <span className="gauge-sub">{facts?.span}</span>
        </Presence>
        {stage === 1 && next && (
          <button className="btn-fill small stage-in" disabled={pending} onClick={() => start(next)}>Start</button>
        )}
      </div>
      <Presence key="actions" show={stage === 0 && next !== null} className="home-actions">
        {next && nextActions(next)}
      </Presence>
      <Presence key="today" show={stage === 1} className="home-today">{today}</Presence>
      {stage === 1 && mobile && tabs}
      {stage === 0 && chevron}
    </div>
  )
}
