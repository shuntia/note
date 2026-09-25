import { gsap } from 'gsap'
import { ScrollTrigger } from 'gsap/ScrollTrigger'
import { Fragment, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { api, ApiError } from '../api'
import { latest } from '../coalesce'
import type { ToastAction } from '../app'
import { DayLine, minutesOf } from '../dayline'
import { eventFacts, nextUp } from '../events'
import { arcPath, Gauge, STROKE, VB, type ArcLine } from '../gauge'
import { makeHold } from '../held'
import { Jot } from '../jot'
import { clampStops, clearTimeline, scrollReveal, scrollToY, scrub, travel, type Timeline, type Trigger } from '../homeMotion'
import { reducedMotion } from '../motion'
import { ghostOut, rise } from '../motion-gsap'
import { NowCounter } from '../nowcounter'
import { Overflow, type OverflowItem } from '../overflow'
import { readPrefs } from '../prefs'
import { eventLabel } from '../receipts'
import {
  effectiveStart,
  elapsedSec,
  isPaused,
  markEnding,
  pausedAt,
  phaseLengthSec,
  phaseStart,
  plannedSec,
  type FocusSession,
} from '../session'
import { SoFar } from '../sofar'
import { Tick } from '../tick'
import type { DayView, PlanEvent, SessionStart, Task, TaskNotify } from '../types'
import { capped } from '../wave'
import { CalendarSection } from './Calendar'
import { Urgent } from './Tasks'
import { DebriefFold } from '../debrief'
import { ReviewFold } from '../review'
import '../styles/home-motion.css'

const LATER_MINUTES = [5, 10, 15, 30, 60]
const laterLabel = (m: number) => (m < 60 ? `${m} min` : `${m / 60} h`)
const ROUTINE_MIN = 15
const PIN_MOBILE = 520
const PIN_DESKTOP = 600
const IDLE_MS = 2000
const WAKE_EVENTS = ['mousemove', 'wheel', 'keydown', 'touchstart', 'pointerdown', 'scroll', 'focusin'] as const

// Drop has no server-side reversal, so the request waits out the undo window.
const dropHold = makeHold<number>()
// Nor does finishing, so the last step's write waits the same way.
const doneHold = makeHold<FocusSession>()

const clamp = (v: number) => Math.min(1, Math.max(0, v))

// A size in pixels at 1440x900, read through the viewport's own scale.
const u = (px: number) => `calc(${px} * var(--u))`

// "Leave them" is the user's answer for the rest of that day, and only on this device.
const LEFT_KEY = 'note.close-day-left'

function leftToday(): boolean {
  try {
    return localStorage.getItem(LEFT_KEY) === todayIso()
  } catch {
    return false
  }
}

const isLive = (t: Task) => t.state === 'open' || t.state === 'in_progress'

// What a block laid for the task does when it starts.
const ANNOUNCE: { id: TaskNotify; label: string }[] = [
  { id: 'none', label: 'None' },
  { id: 'chat', label: 'Chat' },
  { id: 'notify', label: 'Notify' },
]

function nowMinutes(): number {
  const d = new Date()
  return d.getHours() * 60 + d.getMinutes()
}

function todayIso(): string {
  const d = new Date()
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

// What the row is called: a block laid for a task carries the task's own name, and
// a block laid for one of its steps is named after the step, the task behind it.
// A title that opens with the task's own category says the course twice over.
function withoutCategory(title: string, category: string): string {
  const prefix = `${category} — `
  if (!category || !title.startsWith(prefix)) return title
  const rest = title.slice(prefix.length).trim()
  return rest === '' ? title : rest
}

function rowParts(ev: PlanEvent): { name: string; of: string | null } {
  if (!ev.task) return { name: eventLabel(ev.kind), of: null }
  const title = withoutCategory(ev.task.title, ev.task.category)
  if (ev.task.step) return { name: ev.task.step, of: title }
  return { name: title, of: null }
}

function minutesOfDayNow(): number {
  return (Date.now() - new Date().setHours(0, 0, 0, 0)) / 60_000
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

// Text the morph can carry word by word.
function Atoms({ text }: { text: string }) {
  return (
    <span className="atoms">
      {text
        .split(/\s+/)
        .filter(Boolean)
        .map((w, i) => (
          <Fragment key={i}>
            {i > 0 && ' '}
            <span className="atom">{w}</span>
          </Fragment>
        ))}
    </span>
  )
}

function useMotion(): boolean {
  const [on, setOn] = useState(() => !reducedMotion())
  useEffect(() => {
    const mq = window.matchMedia('(prefers-reduced-motion: reduce)')
    const sync = () => setOn(!mq.matches)
    mq.addEventListener('change', sync)
    return () => mq.removeEventListener('change', sync)
  }, [])
  return on
}

// Desktop rests its buttons and the top bar after two idle seconds; any sign of a
// hand brings them back (the CSS reads `html.idle`).
function useIdle(on: boolean) {
  useEffect(() => {
    if (!on) return
    const root = document.documentElement
    let timer = 0
    const wake = () => {
      root.classList.remove('idle')
      window.clearTimeout(timer)
      timer = window.setTimeout(() => root.classList.add('idle'), IDLE_MS)
    }
    for (const ev of WAKE_EVENTS) addEventListener(ev, wake, { passive: true })
    wake()
    return () => {
      window.clearTimeout(timer)
      root.classList.remove('idle')
      for (const ev of WAKE_EVENTS) removeEventListener(ev, wake)
    }
  }, [on])
}

const isTyping = (target: EventTarget | null) => {
  const el = target as HTMLElement | null
  const tag = el?.tagName
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || !!el?.isContentEditable
}

export function Home({
  session,
  setSession,
  notify,
  onChanged,
  refresh,
  openNow,
  openTalk,
  openConversation,
  mobile,
  armed,
}: {
  session: FocusSession | null
  setSession: (s: FocusSession | null) => void
  notify: (msg: string, action?: ToastAction) => void
  onChanged: () => void
  refresh: number
  openNow: (fields: SessionStart) => void
  openTalk: (draft: string) => void
  openConversation: (id: number) => void
  mobile: boolean
  // The shell holds the pin off while a view transition is under way: the layer is
  // transformed then, which no fixed position inside it would survive.
  armed: boolean
}) {
  const [day, setDay] = useState<DayView | null>(null)
  const [beat, tick] = useState(0)
  const [pending, setPending] = useState(false)
  const [left, setLeft] = useState(leftToday)
  const inSession = session !== null
  const prefs = readPrefs()
  const motion = useMotion()
  useIdle(!mobile)

  const newest = useState(() => latest<DayView>())[0]
  const load = useCallback(() => {
    const date = todayIso()
    newest(api.day(date))
      .then((d) => d && setDay(d))
      .catch(() =>
        setDay({ date, events: [], calendar: [], free: [], quiet_now: null, history: [] }),
      )
  }, [newest])
  useEffect(load, [load, refresh])
  const events = day?.events ?? null

  useEffect(() => {
    const id = setInterval(() => tick((n) => n + 1), 1000)
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

  // A block laid for a task is finished on both counts at once: the plan settles,
  // and so does the task it holds.
  const finishBlock = (ev: PlanEvent) =>
    act(async () => {
      await api.eventAction(ev.id, 'done')
      if (ev.task) await api.patchTask(ev.task.id, { state: 'done' })
    })

  // A trigger is Note's own moment to look again, never the user's, so it stays off
  // the day entirely. The beat is a dependency because the holds live outside React state.
  const visible = useMemo(
    () =>
      (events ?? []).filter(
        (ev) => ev.kind !== 'trigger' && ev.id !== dropHold.held() && ev.id !== doneHold.held()?.event_id,
      ),
    [events, beat],
  )
  const now = nowMinutes()
  const next = nextUp(visible, now)
  const [hoverId, setHoverId] = useState<number | null>(null)
  const facts = next ? eventFacts(next, now) : null
  const face = next ? rowParts(next) : null
  const label = face?.name ?? ''

  // A routine is timed to its span; without an end the routine default stands in.
  // A block laid for a task runs as that task, so finishing it settles both.
  const start = (ev: PlanEvent) => {
    home.current
      ?.querySelectorAll('.home-actions, .today-actions, .home-face.compact .btn-fill')
      .forEach(ghostOut)
    const span = ev.end_wall_time
      ? Math.max(1, minutesOf(ev.end_wall_time) - minutesOf(ev.wall_time))
      : ROUTINE_MIN
    openNow({
      title: rowParts(ev).name,
      ...(ev.task && { task_id: ev.task.id }),
      event_id: ev.id,
      planned_min: span,
    })
  }

  // ── session ──────────────────────────────────────────────────
  // Every session route answers with the session itself; the reply is the face.
  const route = async (fn: () => Promise<FocusSession>) => {
    if (pending) return
    setPending(true)
    try {
      setSession(await fn())
    } catch {
      notify("Couldn't reach Note. Try again.")
    } finally {
      setPending(false)
    }
  }

  const pause = () => session && void route(() => api.pauseWorkSession(session.id))
  const resume = () => session && void route(() => api.resumeWorkSession(session.id))
  const backToIt = () => session && void route(() => api.skipBreak(session.id))

  // Ending closes the session at once and holds the writes, so Undo is a toast
  // rather than a question asked before the fact.
  const complete = (s: FocusSession, task: Task | null) => {
    const elapsed = elapsedSec(s)
    markEnding(s.id)
    const send = () => {
      const settled = () => {
        markEnding(null)
        onChanged()
      }
      api.endWorkSession(s.id, 'done').then(settled, settled)
      if (task) {
        api
          .patchTask(task.id, { state: 'done', notes: withElapsedNote(task.notes, elapsed) })
          .then(onChanged)
          .catch(() => notify("Couldn't save the session. Try again."))
      }
      if (s.event_id !== null) {
        api
          .eventAction(s.event_id, 'done')
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
        markEnding(null)
        tick((n) => n + 1)
        setSession(s)
      },
    })
  }

  // The step's own write waits out the undo window, and undoing it hands the
  // session back to the step it was on.
  const advance = (s: FocusSession, step: Task) => {
    const notes = withElapsedNote(step.notes, elapsedSec(s))
    doneHold.start(s, () => {
      api
        .patchTask(step.id, { state: 'done', notes })
        .then(onChanged)
        .catch(() => notify("Couldn't save the session. Try again."))
    })
    onChanged()
    notify('Done', {
      label: 'Undo',
      run: () => {
        if (!doneHold.cancel(s)) return
        void route(() =>
          api.stepWorkSession(s.id, {
            step_index: s.step_index ?? 1,
            step_name: s.step_name ?? s.title,
          }),
        )
      },
    })
  }

  // The step the session is on, and whatever is still open after it.
  const finish = async () => {
    const s = session
    if (!s || pending) return
    setPending(true)
    try {
      const nodes = s.task_id === null ? [] : await api.tasks()
      const parent = nodes.find((n) => n.id === s.task_id) ?? null
      const own = nodes.flatMap((n) => [n as Task, ...n.children]).find((t) => t.id === s.task_id) ?? null
      const steps = parent?.children ?? []
      const at = s.step_index
      const current = at !== null && steps.length ? (steps[at - 1] ?? own) : own
      const next = at !== null ? (steps.slice(at).find(isLive) ?? null) : null
      if (next && current) {
        setSession(
          await api.stepWorkSession(s.id, {
            step_index: steps.indexOf(next) + 1,
            step_name: next.title,
          }),
        )
        advance(s, current)
      } else {
        complete(s, current)
      }
    } catch {
      notify("Couldn't save the session. Try again.")
    } finally {
      setPending(false)
    }
  }

  const pomodoro = session?.mode === 'pomodoro'
  const onBreak = pomodoro && session?.phase === 'break'
  const phaseSec = session && pomodoro ? phaseLengthSec(session) : null
  const planned = session ? plannedSec(session) : null
  // Past the planned end the counter leaves the preference behind and counts the
  // overrun up; a pomodoro round is the server's to end, so it never runs over.
  const over = session !== null && !pomodoro && planned !== null && elapsedSec(session) > planned

  const counter = (s: FocusSession) => {
    const total = pomodoro ? phaseSec : planned
    return (
      <NowCounter
        startedAt={pomodoro ? phaseStart(s) : effectiveStart(s) + (over ? (total ?? 0) * 1000 : 0)}
        durationSec={total ?? 0}
        mode={pomodoro ? 'remaining' : over || total === null ? 'elapsed' : prefs.counter}
        pausedAt={pausedAt(s)}
      />
    )
  }

  const sessionFracAt = (s: FocusSession) => () => {
    const total = pomodoro ? phaseSec : planned
    if (!total) return 0
    const from = pomodoro ? phaseStart(s) : effectiveStart(s)
    return ((pausedAt(s) ?? Date.now()) - from) / (total * 1000)
  }

  const sessionNum = (s: FocusSession, size: number) => (
    <div className={`gauge-num${over ? ' over' : ''}`} style={{ fontSize: u(size) }}>
      {over ? '+' : ''}
      {counter(s)}
    </div>
  )

  const eyebrow = (s: FocusSession) =>
    s.mode !== 'pomodoro' ? null : s.phase === 'break' ? 'BREAK' : `ROUND ${s.round}`

  const faceName = (s: FocusSession) =>
    onBreak ? `Round ${s.round} done` : (s.step_name ?? s.title)

  const stepOf = (s: FocusSession) =>
    s.step_index === null || onBreak ? null : `${s.step_index} of ${s.step_count}`

  const pauseButton = (s: FocusSession) => (
    <button className="btn-round" aria-label={isPaused(s) ? 'Back to it' : 'Break'} onClick={isPaused(s) ? resume : pause}>
      {isPaused(s) ? (
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 5.5v13l10-6.5z" /></svg>
      ) : (
        <svg viewBox="0 0 24 24" aria-hidden="true"><rect x="6" y="5" width="4" height="14" rx="1.2" /><rect x="14" y="5" width="4" height="14" rx="1.2" /></svg>
      )}
    </button>
  )

  const doneButton = (
    <button className={`btn-fill${mobile ? ' wide' : ''}`} disabled={pending} onClick={() => void finish()}>Done with this step</button>
  )

  // The break asks for a word about the round, in the session's own thread.
  const breakSheet = session && onBreak && (
    <div className="break-sheet">
      <button className="btn-haze" disabled={pending} onClick={backToIt}>Back to it</button>
      <Jot
        flow
        conversationId={session.conversation_id}
        placeholder="How did that round go?"
        openTalk={openTalk}
        openConversation={openConversation}
      />
    </div>
  )

  // ── the wait ─────────────────────────────────────────────────
  const from = waitStart(visible, now)
  const waitFracAt = (ev: PlanEvent) => () => {
    if (eventFacts(ev, now).minutes === null) return 1
    const to = minutesOf(ev.wall_time)
    return to <= from ? 1 : clamp((minutesOfDayNow() - from) / (to - from))
  }

  // What a block offers wherever it appears: the face, a list row, the calendar grid
  // and the line all hand out this one list.
  const laterItems = (ev: PlanEvent): OverflowItem[] =>
    LATER_MINUTES.map((m) => ({
      label: laterLabel(m),
      run: () => act(() => api.snooze(ev.id, m)),
      disabled: pending,
    }))

  const blockMenuItems = (ev: PlanEvent): OverflowItem[] => {
    if (ev.status === 'done' || ev.status === 'dropped') {
      return [{ label: ev.status === 'done' ? 'Already done' : 'Already dropped', disabled: true }]
    }
    const tomorrow: OverflowItem = {
      label: 'Move to tomorrow',
      run: () => act(() => api.moveTomorrow(ev.id)),
      disabled: pending,
    }
    const dropToday: OverflowItem = {
      label: 'Drop today',
      kind: 'danger',
      run: () => drop(ev),
      disabled: pending,
    }
    const task = ev.task
    if (task) {
      return [
        { label: 'Start', run: () => start(ev), disabled: pending },
        { label: 'Done', run: () => finishBlock(ev), disabled: pending },
        tomorrow,
        dropToday,
        {
          label: 'Announce',
          children: ANNOUNCE.map((choice) => ({
            label: choice.label,
            run: () => act(() => api.patchTask(task.id, { notify: choice.id })),
            disabled: pending,
            checked: task.notify === undefined ? undefined : task.notify === choice.id,
          })),
        },
      ]
    }
    return [
      { label: 'Start', run: () => start(ev), disabled: pending },
      { label: 'Later', children: laterItems(ev) },
      {
        label: 'Ping me',
        run: () => act(() => api.setEventAlert(ev.id, !ev.alert)),
        disabled: pending,
        checked: ev.alert,
      },
      tomorrow,
      dropToday,
    ]
  }

  const nextActions = (ev: PlanEvent) => (
    <>
      <button className="btn-fill" disabled={pending} onClick={() => start(ev)}>Start</button>
      {ev.task ? (
        <button className="btn-haze" disabled={pending} onClick={() => finishBlock(ev)}>Done</button>
      ) : (
        <Overflow label="Later" className="ev-more-wrap later-wrap" items={laterItems(ev)} />
      )}
      <Overflow label="More" className="ev-more-wrap" items={blockMenuItems(ev)} />
    </>
  )

  // ── the two faces ────────────────────────────────────────────
  const bigFace = session ? (
    <div className="home-face">
      <Gauge size={mobile ? 320 : 440} fracAt={sessionFracAt(session)} breathe paused={isPaused(session)}>
        {eyebrow(session) && <div className="gauge-eyebrow"><Atoms text={eyebrow(session) ?? ''} /></div>}
        {sessionNum(session, mobile ? 64 : 76)}
        <div className="gauge-name" style={{ fontSize: u(mobile ? 20 : 22) }}><Atoms text={faceName(session)} /></div>
        {stepOf(session) && <div className="gauge-sub"><Atoms text={stepOf(session) ?? ''} /></div>}
      </Gauge>
      {breakSheet}
    </div>
  ) : (
    <div className="home-face">
      {events === null ? (
        <Gauge size={mobile ? 320 : 440} faded />
      ) : next && facts ? (
        prefs.showArc ? (
          <Gauge size={mobile ? 320 : 440} fracAt={waitFracAt(next)} faded>
            <div className="gauge-eyebrow"><Atoms text={facts.eyebrow} /></div>
            {facts.wait && (
              <div className="gauge-num" style={{ fontSize: u(mobile ? 50 : 58) }}><Atoms text={facts.wait} /></div>
            )}
            <div className="gauge-name" style={{ fontSize: u(mobile ? 18 : 22) }}><Atoms text={label} /></div>
            {face?.of && <div className="gauge-of"><Atoms text={face.of} /></div>}
            <div className="gauge-sub" style={{ fontSize: mobile ? undefined : u(14) }}><Atoms text={facts.span} /></div>
          </Gauge>
        ) : (
          <div className="home-text">
            <div className="gauge-eyebrow"><Atoms text={facts.eyebrow} /></div>
            <div className="home-title"><Atoms text={label} /></div>
            {face?.of && <div className="gauge-of"><Atoms text={face.of} /></div>}
            {facts.wait && (
              <div className="gauge-num" style={{ fontSize: u(30) }}><Atoms text={`in ${facts.wait}`} /></div>
            )}
            <div className="gauge-sub"><Atoms text={facts.span} /></div>
          </div>
        )
      ) : (
        <div className="home-text"><div className="home-title"><Atoms text="That's everything today." /></div></div>
      )}
    </div>
  )

  // The wrapper is what rests when the desktop goes idle; the group inside is what the morph moves.
  const bigActions = session ? null : next && <div className="rest"><div className="home-actions">{nextActions(next)}</div></div>

  // Mobile, and any session, land on the compact header; the desktop wait lands on
  // the hero.
  const compactHeader = session ? (
    mobile ? (
      <div className="home-face compact session">
        <div className="session-arc">
          <Gauge size={230} fracAt={sessionFracAt(session)} breathe paused={isPaused(session)}>
            {eyebrow(session) && <span className="gauge-eyebrow">{eyebrow(session)}</span>}
            {sessionNum(session, 40)}
            <div className="gauge-name" style={{ fontSize: u(15) }}>{faceName(session)}</div>
            {stepOf(session) && <span className="gauge-sub">{stepOf(session)}</span>}
          </Gauge>
          {pauseButton(session)}
        </div>
      </div>
    ) : (
      <div className="home-face compact">
        <Gauge size={120} fracAt={sessionFracAt(session)} breathe paused={isPaused(session)}>
          {sessionNum(session, 24)}
        </Gauge>
        <div className="home-head">
          {eyebrow(session) && <span className="gauge-eyebrow">{eyebrow(session)}</span>}
          <span className="home-head-name">{faceName(session)}</span>
          {stepOf(session) && <span className="gauge-sub">{stepOf(session)}</span>}
        </div>
        {doneButton}
        {pauseButton(session)}
      </div>
    )
  ) : next && facts ? (
    <div className="home-face compact">
      {prefs.showArc ? (
        <Gauge size={120} fracAt={waitFracAt(next)} faded>
          {facts.wait && <span className="gauge-num" style={{ fontSize: u(22) }}>{facts.wait}</span>}
        </Gauge>
      ) : (
        facts.wait && <span className="gauge-num" style={{ fontSize: u(22) }}>{facts.wait}</span>
      )}
      <div className="home-head">
        <span className="gauge-eyebrow">{facts.eyebrow}</span>
        <span className="home-head-name">{label}</span>
        {face?.of && <span className="gauge-of">{face.of}</span>}
        <span className="gauge-sub">{facts.span}</span>
      </div>
      <div className="home-head-actions">
        <button className="btn-fill small" disabled={pending} onClick={() => start(next)}>Start</button>
        {next.task ? (
          <button className="btn-haze small" disabled={pending} onClick={() => finishBlock(next)}>Done</button>
        ) : (
          <Overflow label="Later" className="ev-more-wrap later-wrap" items={laterItems(next)} />
        )}
        <Overflow label="More" className="ev-more-wrap" items={blockMenuItems(next)} />
      </div>
    </div>
  ) : null

  const hero = (
    <section className="today-hero">
      {next && facts ? (
        <>
          <div className="today-eyebrow">{facts.eyebrow} {next.wall_time}</div>
          <h1 className="today-title">{label}</h1>
          {face?.of && <p className="today-of">{face.of}</p>}
          <div className="today-wait">
            <div className="today-when">
              {facts.wait && (
                <span className="today-in"><span className="in-word">in</span><span className="in-num">{facts.wait}</span></span>
              )}
              <span className="today-span">{facts.span}</span>
            </div>
            <div className="today-bar" aria-hidden="true"><span className="today-bar-fill" style={{ width: `${waitFracAt(next)() * 100}%` }} /></div>
          </div>
          <div className="rest"><div className="today-actions">{nextActions(next)}</div></div>
        </>
      ) : (
        events && <h1 className="today-title">That's everything today.</h1>
      )}
    </section>
  )

  // The whole day in time order: what the plan holds and every window the calendar
  // has, the ones already over among them.
  const rows = useMemo(() => {
    const plan = visible
      .filter((ev) => ev.status !== 'dropped')
      .map((ev) => ({
        at: minutesOf(ev.wall_time),
        to: minutesOf(ev.end_wall_time ?? ev.wall_time),
        ev,
      }))
    const cal = (day?.calendar ?? []).map((occ) => ({
      at: minutesOf(occ.start),
      to: minutesOf(occ.end),
      occ,
    }))
    return [...plan, ...cal].sort((a, b) => a.at - b.at || a.to - b.to)
  }, [visible, day])

  const openBlocks = useMemo(
    () => visible.filter((ev) => ev.task && ev.status !== 'done' && ev.status !== 'dropped'),
    [visible],
  )

  const leaveThem = () => {
    setLeft(true)
    try {
      localStorage.setItem(LEFT_KEY, todayIso())
    } catch {
      // storage blocked; the card stays gone for this session
    }
  }

  const pastCloseDay = prefs.closeDay !== '' && now >= minutesOf(prefs.closeDay)
  const closeDay = pastCloseDay && !left && openBlocks.length > 0 && (
    <section className="close-day">
      <p className="close-day-head">Close the day</p>
      <p className="close-day-line">
        {openBlocks.length} block{openBlocks.length === 1 ? '' : 's'} still open.
      </p>
      <div className="close-day-actions">
        <button
          className="btn-fill small"
          disabled={pending}
          onClick={() => act(() => api.carry(todayIso()))}
        >
          Carry to tomorrow
        </button>
        <button className="btn-haze small" disabled={pending} onClick={leaveThem}>
          Leave them
        </button>
      </div>
    </section>
  )

  const planRow = (ev: PlanEvent) => {
    const { name, of } = rowParts(ev)
    return (
      <>
        <span className="home-when">{ev.wall_time}<span className="home-when-end"> – {ev.end_wall_time ?? ev.wall_time}</span></span>
        {ev.task && (
          <Tick
            checked={ev.status === 'done'}
            label={`Done: ${name}`}
            onClick={() => finishBlock(ev)}
          />
        )}
        <span className="home-what">
          <span className="home-name">{name}</span>
          {ev.task && <Urgent task={{ ...ev.task, due_at: null }} />}
          {of && <span className="home-of">{of}</span>}
        </span>
        <button className="home-start" aria-label={`Start ${name}`} disabled={pending} onClick={() => start(ev)}>
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M9 7.5v9l7-4.5z" />
          </svg>
        </button>
        <Overflow label={`More: ${name}`} row=".home-list li" items={blockMenuItems(ev)} />
      </>
    )
  }

  const list = (
    <ul className="home-list" onMouseLeave={() => setHoverId(null)}>
      {rows.map((row) =>
        'ev' in row ? (
          <li
            key={`e${row.ev.id}`}
            title={row.ev.prompt}
            onMouseEnter={() => setHoverId(row.ev.id)}
            className={[
              row.ev.id === next?.id ? 'next' : '',
              row.ev.id === hoverId ? 'hover' : '',
              row.ev.task ? 'task' : '',
              row.to <= now ? 'past' : '',
              row.to < now && row.ev.status !== 'done' ? 'overdue' : '',
            ]
              .filter(Boolean)
              .join(' ')}
          >
            {planRow(row.ev)}
          </li>
        ) : (
          <li
            key={`c${row.occ.entry_id}-${row.occ.start}`}
            className={`cal ${row.occ.kind}${row.to <= now ? ' past' : row.at <= now ? ' on' : ''}`}
          >
            <span className="home-when">{row.occ.start}<span className="home-when-end"> – {row.occ.end}</span></span>
            <span className="home-what">{row.occ.title}</span>
          </li>
        ),
      )}
    </ul>
  )

  const compactLanding = mobile || inSession
  // On the phone a session is the whole screen: the day waits until it is over.
  const today = day && !(mobile && inSession) && (
    <div className="home-today">
      <DayLine events={visible} calendar={day.calendar} now={now} compact={mobile} nextId={next?.id} hoverId={hoverId} onHover={setHoverId} />
      {closeDay}
      {list}
      {mobile && <SoFar rows={day.history} />}
      {mobile && <Jot flow openTalk={openTalk} openConversation={openConversation} />}
    </div>
  )

  // ── the choreography ─────────────────────────────────────────
  const home = useRef<HTMLDivElement>(null)
  const stage = useRef<HTMLElement>(null)
  const ground = useRef<HTMLElement>(null)
  const tl = useRef<Timeline | null>(null)
  const st = useRef<Trigger | null>(null)
  const [ready, setReady] = useState(false)
  const choreo = motion && !!events

  const distance = mobile ? PIN_MOBILE : PIN_DESKTOP
  useLayoutEffect(() => {
    if (!motion || !armed) return
    const el = stage.current
    const root = home.current
    if (!el || !root) return
    const topbar = mobile ? null : document.querySelector<HTMLElement>('.shell .topbar')
    const trigger = scrub(
      () => tl.current,
      { trigger: el, start: topbar ? `top ${topbar.offsetHeight}px` : 'top top', end: `+=${distance}`, pin: true, pinSpacing: true, anticipatePin: 1 },
      { down: 0.7, up: 1.2 },
    )
    st.current = trigger
    const barPin = topbar
      ? ScrollTrigger.create({ trigger: topbar, start: 'top top', end: `+=${distance}`, pin: true, pinSpacing: false })
      : null
    // `?nosnap` holds a mid frame for screenshots.
    const unsnap = new URLSearchParams(location.search).has('nosnap') ? () => {} : clampStops(() => trigger)
    const onKey = (e: KeyboardEvent) => {
      if (isTyping(e.target) || e.defaultPrevented) return
      if (e.key === 'ArrowDown' && window.scrollY < trigger.end) {
        e.preventDefault()
        scrollToY(trigger.end)
      } else if (e.key === 'ArrowUp' && window.scrollY <= trigger.end + 1) {
        e.preventDefault()
        scrollToY(trigger.start)
      }
    }
    addEventListener('keydown', onKey)
    // Content that arrives later (the plan, the letter, the calendar) changes the page's height.
    let raf = 0
    const ro = new ResizeObserver(() => {
      cancelAnimationFrame(raf)
      raf = requestAnimationFrame(() => ScrollTrigger.refresh())
    })
    ro.observe(root)
    return () => {
      ro.disconnect()
      cancelAnimationFrame(raf)
      removeEventListener('keydown', onKey)
      unsnap()
      barPin?.kill()
      trigger.kill()
      st.current = null
    }
  }, [motion, mobile, distance, armed])

  // Without the morph there is no pin, but the face and the day are still the two
  // stops a gesture moves between.
  useLayoutEffect(() => {
    if (motion || !armed) return
    const face = home.current?.querySelector<HTMLElement>('.face-still')
    if (!face) return
    return clampStops(() => ({ start: 0, end: face.offsetHeight }))
  }, [motion, armed])

  // Every landing spot is measured from rendered text, so the timeline is rebuilt
  // whenever what is on the face or where it lands could have moved.
  const shape = [
    choreo,
    compactLanding,
    prefs.showArc,
    next?.id,
    label,
    facts?.eyebrow,
    facts?.wait,
    facts?.span,
    face?.of,
    session?.started_at,
    session?.step_index,
    session?.step_name,
    session?.phase,
    session?.round,
    over,
    visible.map((ev) => ev.id).join(','),
  ].join('|')
  const [layout, relayout] = useState(0)
  useEffect(() => {
    if (!motion) return
    const bump = () => relayout((n) => n + 1)
    ScrollTrigger.addEventListener('refresh', bump)
    return () => ScrollTrigger.removeEventListener('refresh', bump)
  }, [motion])

  useLayoutEffect(() => {
    if (!choreo) {
      setReady(false)
      return
    }
    let stale = false
    const build = () => {
      if (stale) return
      const el = stage.current
      if (!el) return
      if (tl.current) clearTimeline(tl.current)
      const timeline = gsap.timeline({ paused: true })
      const q = (sel: string) => el.querySelector<HTMLElement>(sel)
      const qa = (sel: string) => [...el.querySelectorAll<HTMLElement>(sel)]
      const to = (targets: HTMLElement[], vars: gsap.TweenVars, at: number) => {
        if (targets.length) timeline.to(targets, vars, at)
      }
      const from = (targets: HTMLElement[], vars: gsap.TweenVars, at: number) => {
        if (targets.length) timeline.from(targets, vars, at)
      }
      if (compactLanding) {
        travel(timeline, q('.face-big .gauge-ring'), q('.home-face.compact .gauge-ring'), { mode: 'box' })
        travel(timeline, q('.face-big .gauge-num'), q('.home-face.compact .gauge-num'), { mode: inSession ? 'box' : 'text' })
        travel(timeline, q('.face-big .gauge-eyebrow'), q('.home-face.compact .gauge-eyebrow'))
        travel(timeline, q('.face-big .gauge-name, .face-big .home-title'), q('.home-face.compact .home-head-name, .home-face.compact .gauge-name'))
        travel(timeline, q('.face-big .gauge-sub'), q('.home-face.compact .home-head .gauge-sub, .home-face.compact .gauge-sub'))
        travel(timeline, q('.face-big .gauge-of'), q('.home-face.compact .home-head .gauge-of'))
        if (inSession) {
          // The controls settle in once the ring has cleared the header row.
          from(qa('.home-face.compact .btn-round, .home-face.compact .btn-fill'), { autoAlpha: 0, scale: 0.85, duration: 0.3, ease: 'power2.out' }, 0.65)
          from(qa('.home-sheet'), { autoAlpha: 0, y: 24, duration: 0.3, ease: 'power2.out' }, 0.62)
        } else {
          travel(timeline, q('.face-big .home-actions .btn-fill'), q('.home-face.compact .btn-fill'), { mode: 'box', fit: 'both' })
          to(qa('.face-big .btn-haze, .face-big .ev-more-wrap'), { autoAlpha: 0, x: -24, y: -10, duration: 0.45, ease: 'power2.in' }, 0.2)
          from(qa('.home-head-actions .btn-haze, .home-head-actions .ev-more-wrap'), { autoAlpha: 0, duration: 0.3, ease: 'power2.out' }, 0.7)
        }
        if (!q('.home-face.compact')) to(qa('.face-big .home-text'), { autoAlpha: 0, y: -20, duration: 0.4, ease: 'power2.in' }, 0.2)
        from(qa('.home-today .dayline'), { autoAlpha: 0, y: 28, duration: 0.4, ease: 'power2.out' }, 0.35)
        from(qa('.home-list li'), { autoAlpha: 0, y: 24, duration: 0.35, ease: 'power2.out', stagger: capped(0.04, qa('.home-list li').length) }, 0.42)
        from(qa('.home-today .close-day, .home-today .sofar'), { autoAlpha: 0, y: 24, duration: 0.35, ease: 'power2.out' }, 0.42)
        from(qa('.home-today .jot-wrap'), { autoAlpha: 0, y: 24, duration: 0.35, ease: 'power2.out' }, 0.5)
      } else {
        const ring = q('.face-big .gauge-ring') as SVGSVGElement | null
        const bar = q('.today-bar')
        if (ring && bar) {
          const paths = [...ring.querySelectorAll('path')]
          const flat = (t: number, line?: ArcLine, width = STROKE) => {
            const d = arcPath(t, line)
            for (const p of paths) {
              p.setAttribute('d', d)
              p.setAttribute('stroke-width', width.toFixed(2))
            }
          }
          flat(0)
          const sr = ring.getBoundingClientRect()
          const br = bar.getBoundingClientRect()
          const k = sr.width / VB
          const line: ArcLine = [(br.left - sr.left) / k, (br.right - sr.left) / k, (br.top + br.height / 2 - sr.top) / k]
          const thin = br.height / k
          const fl = { t: 0 }
          timeline
            .to(fl, { t: 1, duration: 1, ease: 'power2.inOut', onUpdate: () => flat(fl.t, line, STROKE + (thin - STROKE) * fl.t) }, 0)
            .to(ring, { autoAlpha: 0, duration: 0.015, ease: 'none' }, 0.985)
            .from(bar, { autoAlpha: 0, duration: 0.015, ease: 'none' }, 0.985)
          from(qa('.in-word'), { autoAlpha: 0, duration: 0.4, ease: 'none' }, 0.6)
        }
        travel(timeline, q('.face-big .gauge-num'), q('.in-num'))
        travel(timeline, q('.face-big .gauge-name, .face-big .home-title'), q('.today-title'))
        travel(timeline, q('.face-big .gauge-sub'), q('.today-span'))
        travel(timeline, q('.face-big .gauge-of'), q('.today-of'))
        travel(timeline, q('.face-big .gauge-eyebrow'), q('.today-eyebrow'))
        travel(timeline, q('.face-big .home-actions'), q('.today-actions'), { mode: 'children', fit: 'both' })
        from(qa('.today-line'), { autoAlpha: 0, y: 28, duration: 0.45, ease: 'power2.out' }, 0.35)
      }
      to(qa('.face-big .chev'), { autoAlpha: 0, duration: 0.35 }, 0)
      tl.current = timeline
      timeline.progress(st.current?.progress ?? 0)
      setReady(true)
    }
    void document.fonts.ready.then(build)
    return () => {
      stale = true
    }
  }, [shape, layout])

  useEffect(
    () => () => {
      if (tl.current) clearTimeline(tl.current)
      tl.current = null
    },
    [],
  )

  // What sits under the stage fades in as it scrolls up, and away again at the top:
  // the letter, then the calendar, whose silhouettes, windows and rows draw
  // themselves in turn. Rebuilt whenever what is under there changes.
  useLayoutEffect(() => {
    if (!motion) return
    const el = ground.current
    if (!el) return
    let undo: (() => void) | null = null
    let raf = 0
    const arm = () => {
      undo?.()
      const qa = (sel: string) => [...el.querySelectorAll<HTMLElement>(sel)]
      const from = (t: Timeline, targets: HTMLElement[], vars: gsap.TweenVars, at: number) => {
        if (targets.length) t.from(targets, vars, at)
      }
      undo = scrollReveal(
        (t) => {
          from(t, qa('.debrief-row, .debrief-note, .sofar'), { autoAlpha: 0, y: 28, duration: 0.8, ease: 'power2.out' }, 0)
          from(t, qa('#calendar-slot'), { autoAlpha: 0, y: 40, duration: 1, ease: 'power2.out' }, mobile ? 0 : 0.25)
          from(t, qa('.ws-seg'), { scaleY: 0, transformOrigin: 'top', duration: 0.6, ease: 'power2.out', stagger: capped(0.04, qa('.ws-seg').length) }, 0.35)
          from(t, qa('.cal-line .cal-band'), { scaleX: 0, transformOrigin: 'left center', duration: 0.6, ease: 'power2.out', stagger: capped(0.1, qa('.cal-line .cal-band').length) }, 0.6)
          from(t, qa('.cal-line .dl-label, .cal-line .dl-now'), { autoAlpha: 0, duration: 0.4 }, 1.0)
          from(t, qa('.cal-list li'), { autoAlpha: 0, y: 10, duration: 0.5, ease: 'power2.out', stagger: capped(0.08, qa('.cal-list li').length) }, 0.85)
          from(t, qa('.week'), { autoAlpha: 0, y: 36, duration: 1, ease: 'power2.out' }, 0.45)
          from(t, qa('.week .band'), { autoAlpha: 0, y: 10, duration: 0.6, ease: 'power2.out', stagger: capped(0.05, qa('.week .band').length) }, 0.9)
        },
        { trigger: el, start: mobile ? 'clamp(top 92%)' : 'clamp(top 88%)', end: mobile ? 'clamp(top 45%)' : 'clamp(top 30%)' },
      )
      ScrollTrigger.refresh()
    }
    arm()
    const mo = new MutationObserver(() => {
      cancelAnimationFrame(raf)
      raf = requestAnimationFrame(arm)
    })
    mo.observe(el, { childList: true, subtree: true })
    return () => {
      mo.disconnect()
      cancelAnimationFrame(raf)
      undo?.()
    }
  }, [motion, mobile])

  // A session starting or ending changes the face, so the page goes back to it,
  // and what the face now says arrives rather than appears.
  useEffect(() => {
    if (window.scrollY > 0) scrollToY(0)
    if (inSession) rise(home.current?.querySelector('.gauge-centre'), 14)
  }, [inSession])

  const chevron = (
    <button className="chev" aria-label="Today" onClick={() => st.current && scrollToY(st.current.end)}>
      <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 14l6-6 6 6" /></svg>
    </button>
  )

  const cls = [
    'home',
    mobile ? 'mobile' : 'desktop',
    inSession ? 'in-session' : '',
    motion ? 'motion' : 'still',
    ready ? 'morph-ready' : '',
    prefs.showArc ? '' : 'no-arc',
  ]
    .filter(Boolean)
    .join(' ')

  return (
    <div ref={home} className={cls}>
      <section ref={stage} className="stage" aria-label="Today">
        {motion ? (
          <div className="face-big">
            {bigFace}
            {bigActions}
            {chevron}
          </div>
        ) : (
          <div className="face-still">
            {bigFace}
            {bigActions}
            {session && <div className="home-sheet">{pauseButton(session)}{doneButton}</div>}
          </div>
        )}
        {motion && (compactLanding ? compactHeader : hero)}
        {motion && session && mobile && (
          <div className="home-sheet">
            {doneButton}
            <Jot flow placeholder="Tell Note" openTalk={openTalk} openConversation={openConversation} />
          </div>
        )}
        {compactLanding ? today : events && <section className="today-line"><DayLine events={visible} calendar={day?.calendar} now={now} nextId={next?.id} hoverId={hoverId} onHover={setHoverId} />{closeDay}{list}</section>}
      </section>
      <section ref={ground} className="today-ground">
        {!mobile && <DebriefFold />}
        {!mobile && <ReviewFold />}
        {!mobile && day && <SoFar rows={day.history} />}
        <section id="calendar-slot">
          <CalendarSection
            notify={notify}
            refresh={refresh}
            onChanged={onChanged}
            day={day}
            blockItems={blockMenuItems}
          />
        </section>
      </section>
    </div>
  )
}
