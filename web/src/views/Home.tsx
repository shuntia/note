import { gsap } from 'gsap'
import { ScrollTrigger } from 'gsap/ScrollTrigger'
import { Fragment, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { api, ApiError } from '../api'
import type { ToastAction } from '../app'
import { DayLine, minutesOf } from '../dayline'
import { useEscape } from '../escape'
import { eventFacts, nextUp } from '../events'
import { arcPath, Gauge, STROKE, VB, type ArcLine } from '../gauge'
import { makeHold } from '../held'
import { clearTimeline, scrollReveal, scrollToY, scrub, snapNearest, travel, type Timeline, type Trigger } from '../homeMotion'
import { reducedMotion } from '../motion'
import { NowCounter } from '../nowcounter'
import { Overflow } from '../overflow'
import { readPrefs } from '../prefs'
import { eventLabel } from '../receipts'
import { effectiveStart, elapsedSec, type FocusSession } from '../session'
import { TellNote } from '../tellnote'
import { SoFar } from '../sofar'
import type { DayView, PlanEvent } from '../types'
import { CalendarSection } from './Calendar'
import { DebriefFold } from '../debrief'
import '../styles/home-motion.css'

const LATER_MINUTES = [5, 10, 15, 30, 60]
const ROUTINE_MIN = 15
const PIN_MOBILE = 520
const PIN_DESKTOP = 600
const IDLE_MS = 2000
const WAKE_EVENTS = ['mousemove', 'wheel', 'keydown', 'touchstart', 'pointerdown', 'scroll', 'focusin'] as const

// Drop has no server-side reversal, so the request waits out the undo window.
const dropHold = makeHold<number>()
// Nor does finishing, so the last step's write waits the same way.
const doneHold = makeHold<FocusSession>()

const round5 = (min: number) => Math.max(5, Math.round(min / 5) * 5)
const clamp = (v: number) => Math.min(1, Math.max(0, v))

function nowMinutes(): number {
  const d = new Date()
  return d.getHours() * 60 + d.getMinutes()
}

function todayIso(): string {
  const d = new Date()
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

// What the row is called: a block laid for a task carries the task's own name.
function rowLabel(ev: PlanEvent): string {
  return ev.task ? ev.task.title : eventLabel(ev.kind)
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
  mobile,
  armed,
}: {
  session: FocusSession | null
  setSession: (s: FocusSession | null) => void
  notify: (msg: string, action?: ToastAction) => void
  onChanged: () => void
  refresh: number
  openNow: (s: FocusSession) => void
  mobile: boolean
  // The shell holds the pin off while a view transition is under way: the layer is
  // transformed then, which no fixed position inside it would survive.
  armed: boolean
}) {
  const [day, setDay] = useState<DayView | null>(null)
  const [beat, tick] = useState(0)
  const [pending, setPending] = useState(false)
  const [later, setLater] = useState(false)
  const inSession = session !== null
  const prefs = readPrefs()
  const motion = useMotion()
  useIdle(!mobile)

  useEscape(later, () => setLater(false))

  const load = useCallback(() => {
    const date = todayIso()
    api
      .day(date)
      .then(setDay)
      .catch(() =>
        setDay({ date, events: [], calendar: [], free: [], quiet_now: null, history: [] }),
      )
  }, [])
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

  // the beat is a dependency because the holds live outside React state
  const visible = useMemo(
    () => (events ?? []).filter((ev) => ev.id !== dropHold.held() && ev.id !== doneHold.held()?.eventId),
    [events, beat],
  )
  const now = nowMinutes()
  const next = nextUp(visible, now)
  const facts = next ? eventFacts(next, now) : null
  const label = next ? eventLabel(next.kind) : ''

  // A routine is timed to its span; without an end the routine default stands in.
  // A block laid for a task runs as that task, so finishing it settles both.
  const start = (ev: PlanEvent) => {
    const span = ev.end_wall_time
      ? Math.max(1, minutesOf(ev.end_wall_time) - minutesOf(ev.wall_time))
      : ROUTINE_MIN
    openNow({
      taskId: ev.task?.id ?? null,
      eventId: ev.id,
      title: rowLabel(ev),
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

  // ── session ──────────────────────────────────────────────────
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
      }
      if (s.eventId !== null) {
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
  // The step's write waits out the undo window, so every Done is reversible.
  const advance = async (s: FocusSession, step: number) => {
    if (pending) return
    setPending(true)
    try {
      const nodes = await api.tasks()
      const parent = nodes.find((n) => n.children.some((c) => c.id === step))
      const at = parent?.children.findIndex((c) => c.id === step) ?? -1
      const open = parent?.children.slice(at + 1).find((c) => c.state === 'open' || c.state === 'in_progress')
      const notes = withElapsedNote(s.notes, elapsedSec(s))
      doneHold.start(s, () => {
        api
          .patchTask(step, { state: 'done', notes })
          .then(onChanged)
          .catch(() => notify("Couldn't save the session. Try again."))
      })
      setSession(
        parent && open
          ? {
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
            }
          : null,
      )
      onChanged()
      notify('Done', {
        label: 'Undo',
        run: () => {
          if (!doneHold.cancel(s)) return
          tick((n) => n + 1)
          setSession(s)
        },
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
  const over = session !== null && session.durationSec !== null && elapsedSec(session) > session.durationSec

  const counter = (s: FocusSession) => {
    const total = s.durationSec
    return (
      <NowCounter
        startedAt={effectiveStart(s) + (over ? (total ?? 0) * 1000 : 0)}
        durationSec={total ?? 0}
        mode={over || total === null ? 'elapsed' : prefs.counter}
        pausedAt={s.pausedAt}
      />
    )
  }

  const sessionFracAt = (s: FocusSession) => () =>
    s.durationSec ? ((s.pausedAt ?? Date.now()) - effectiveStart(s)) / (s.durationSec * 1000) : 0

  const sessionNum = (s: FocusSession, size: number) => (
    <div className={`gauge-num${over ? ' over' : ''}`} style={{ fontSize: size }}>
      {over ? '+' : ''}
      {counter(s)}
    </div>
  )

  const pauseButton = (s: FocusSession) => (
    <button className="btn-round" aria-label={s.pausedAt ? 'Back to it' : 'Break'} onClick={s.pausedAt ? resume : pause}>
      {s.pausedAt ? (
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 5.5v13l10-6.5z" /></svg>
      ) : (
        <svg viewBox="0 0 24 24" aria-hidden="true"><rect x="6" y="5" width="4" height="14" rx="1.2" /><rect x="14" y="5" width="4" height="14" rx="1.2" /></svg>
      )}
    </button>
  )

  const doneButton = (
    <button className={`btn-fill${mobile ? ' wide' : ''}`} disabled={pending} onClick={finish}>Done with this step</button>
  )

  // ── the wait ─────────────────────────────────────────────────
  const from = waitStart(visible, now)
  const waitFracAt = (ev: PlanEvent) => () => {
    if (eventFacts(ev, now).minutes === null) return 1
    const to = minutesOf(ev.wall_time)
    return to <= from ? 1 : clamp((minutesOfDayNow() - from) / (to - from))
  }

  const nextActions = (ev: PlanEvent) => (
    <>
      <button className="btn-fill" disabled={pending} onClick={() => start(ev)}>Start</button>
      <button className="btn-haze" aria-expanded={later} disabled={pending} onClick={() => setLater((v) => !v)}>Later</button>
      <Overflow
        label="More"
        className={`ev-more-wrap${later ? ' beside-later' : ''}`}
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

  // ── the two faces ────────────────────────────────────────────
  const bigFace = session ? (
    <div className="home-face">
      <Gauge size={mobile ? 320 : 440} fracAt={sessionFracAt(session)} breathe paused={session.pausedAt !== null}>
        {sessionNum(session, 58)}
        <div className="gauge-name" style={{ fontSize: 18 }}><Atoms text={session.stepName ?? session.title} /></div>
        {session.stepIndex !== null && (
          <div className="gauge-sub"><Atoms text={`${session.stepIndex} of ${session.stepCount}`} /></div>
        )}
      </Gauge>
    </div>
  ) : (
    <div className="home-face">
      {events === null ? (
        <Gauge size={mobile ? 320 : 440} faded />
      ) : next && facts ? (
        prefs.showArc ? (
          <Gauge size={mobile ? 320 : 440} fracAt={waitFracAt(next)} faded>
            <div className="gauge-eyebrow"><Atoms text={facts.eyebrow} /></div>
            {facts.minutes !== null && (
              <div className="gauge-num" style={{ fontSize: mobile ? 50 : 58 }}><Atoms text={`${facts.minutes} min`} /></div>
            )}
            <div className="gauge-name" style={{ fontSize: mobile ? 18 : 22 }}><Atoms text={label} /></div>
            <div className="gauge-sub" style={{ fontSize: mobile ? undefined : '0.875rem' }}><Atoms text={facts.span} /></div>
          </Gauge>
        ) : (
          <div className="home-text">
            <div className="gauge-eyebrow"><Atoms text={facts.eyebrow} /></div>
            <div className="home-title"><Atoms text={label} /></div>
            {facts.minutes !== null && (
              <div className="gauge-num" style={{ fontSize: 30 }}><Atoms text={`in ${facts.minutes} min`} /></div>
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
    <div className="home-face compact">
      <Gauge size={120} fracAt={sessionFracAt(session)} breathe paused={session.pausedAt !== null}>
        {sessionNum(session, 24)}
      </Gauge>
      <div className="home-head">
        <span className="home-head-name">{session.stepName ?? session.title}</span>
        {session.stepIndex !== null && <span className="gauge-sub">{session.stepIndex} of {session.stepCount}</span>}
      </div>
      {!mobile && doneButton}
      {pauseButton(session)}
    </div>
  ) : next && facts ? (
    <div className="home-face compact">
      {prefs.showArc ? (
        <Gauge size={120} fracAt={waitFracAt(next)} faded>
          {facts.minutes !== null && <span className="gauge-num" style={{ fontSize: 22 }}>{facts.minutes} min</span>}
        </Gauge>
      ) : (
        facts.minutes !== null && <span className="gauge-num" style={{ fontSize: 22 }}>{facts.minutes} min</span>
      )}
      <div className="home-head">
        <span className="gauge-eyebrow">{facts.eyebrow}</span>
        <span className="home-head-name">{label}</span>
        <span className="gauge-sub">{facts.span}</span>
      </div>
      <button className="btn-fill small" disabled={pending} onClick={() => start(next)}>Start</button>
    </div>
  ) : null

  const nowLabel = `${String(Math.floor(now / 60)).padStart(2, '0')}:${String(now % 60).padStart(2, '0')}`
  const hero = (
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
          <h1 className="today-title">{label}</h1>
          <div className="today-wait">
            <div className="today-when">
              {facts.minutes !== null && (
                <span className="today-in"><span className="in-word">in</span><span className="in-num">{facts.minutes} min</span></span>
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

  // Everything still ahead, plus anything that pinged and was never answered.
  const upcoming = visible.filter(
    (ev) =>
      (ev.status === 'pending' || ev.status === 'snoozed' || ev.status === 'fired') &&
      (minutesOf(ev.end_wall_time ?? ev.wall_time) >= now || ev.status === 'fired'),
  )

  const blockActions = (ev: PlanEvent) => (
    <span className="home-row-actions">
      <button className="btn-haze small" disabled={pending} onClick={() => start(ev)}>Start</button>
      <button className="btn-haze small" disabled={pending} onClick={() => finishBlock(ev)}>Done</button>
      <Overflow
        label="More"
        items={[
          { label: 'Drop today', run: () => drop(ev), disabled: pending },
          { label: 'Move to tomorrow', run: () => act(() => api.moveTomorrow(ev.id)), disabled: pending },
        ]}
      />
    </span>
  )

  const list = (
    <ul className="home-list">
      {upcoming.map((ev) => (
        <li
          key={ev.id}
          className={[
            ev.id === next?.id ? 'next' : '',
            ev.task ? 'task' : '',
            minutesOf(ev.end_wall_time ?? ev.wall_time) < now ? 'overdue' : '',
          ]
            .filter(Boolean)
            .join(' ')}
        >
          <span className="home-when">{ev.wall_time} – {ev.end_wall_time ?? ev.wall_time}</span>
          <span className="home-what">{rowLabel(ev)}</span>
          {ev.task && blockActions(ev)}
        </li>
      ))}
    </ul>
  )

  const compactLanding = mobile || inSession
  const today = day && (
    <div className="home-today">
      <DayLine events={visible} now={now} compact={mobile} nextId={next?.id} />
      {list}
      {mobile && <SoFar rows={day.history} />}
      {mobile && <TellNote notify={notify} />}
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
      { trigger: el, start: topbar ? 'top 64px' : 'top top', end: `+=${distance}`, pin: true, pinSpacing: true, anticipatePin: 1 },
      { down: 0.7, up: 1.2 },
    )
    st.current = trigger
    const barPin = topbar
      ? ScrollTrigger.create({ trigger: topbar, start: 'top top', end: `+=${distance}`, pin: true, pinSpacing: false })
      : null
    // `?nosnap` holds a mid frame for screenshots.
    const unsnap = new URLSearchParams(location.search).has('nosnap') ? () => {} : snapNearest(trigger)
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

  // Every landing spot is measured from rendered text, so the timeline is rebuilt
  // whenever what is on the face or where it lands could have moved.
  const shape = [
    choreo,
    compactLanding,
    prefs.showArc,
    next?.id,
    label,
    facts?.eyebrow,
    facts?.minutes,
    facts?.span,
    session?.startedAt,
    session?.stepIndex,
    session?.stepName,
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
      const tabsEl = document.querySelectorAll<HTMLElement>('.shell > .tabs')
      if (compactLanding) {
        travel(timeline, q('.face-big .gauge-ring'), q('.home-face.compact .gauge-ring'), { mode: 'box' })
        travel(timeline, q('.face-big .gauge-num'), q('.home-face.compact .gauge-num'), { mode: inSession ? 'box' : 'text' })
        travel(timeline, q('.face-big .gauge-eyebrow'), q('.home-face.compact .gauge-eyebrow'))
        travel(timeline, q('.face-big .gauge-name, .face-big .home-title'), q('.home-face.compact .home-head-name'))
        travel(timeline, q('.face-big .gauge-sub'), q('.home-face.compact .home-head .gauge-sub'))
        if (inSession) {
          // The controls settle in once the ring has cleared the header row.
          from(qa('.home-face.compact .btn-round, .home-face.compact .btn-fill'), { autoAlpha: 0, scale: 0.85, duration: 0.3, ease: 'power2.out' }, 0.65)
          from(qa('.home-sheet'), { autoAlpha: 0, y: 24, duration: 0.3, ease: 'power2.out' }, 0.62)
        } else {
          travel(timeline, q('.face-big .home-actions .btn-fill'), q('.home-face.compact .btn-fill'), { mode: 'box', fit: 'both' })
          to(qa('.face-big .btn-haze, .face-big .ev-more-wrap'), { autoAlpha: 0, x: -24, y: -10, duration: 0.45, ease: 'power2.in' }, 0.2)
        }
        if (!q('.home-face.compact')) to(qa('.face-big .home-text'), { autoAlpha: 0, y: -20, duration: 0.4, ease: 'power2.in' }, 0.2)
        from(qa('.home-today .dayline'), { autoAlpha: 0, y: 28, duration: 0.4, ease: 'power2.out' }, 0.35)
        from(qa('.home-list li'), { autoAlpha: 0, y: 24, duration: 0.35, ease: 'power2.out', stagger: 0.04 }, 0.42)
        from(qa('.home-today .tellnote-wrap'), { autoAlpha: 0, y: 24, duration: 0.35, ease: 'power2.out' }, 0.5)
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
        travel(timeline, q('.face-big .gauge-eyebrow'), q('.today-eyebrow'))
        travel(timeline, q('.face-big .home-actions'), q('.today-actions'), { mode: 'children', fit: 'both' })
        from(qa('.today-line'), { autoAlpha: 0, y: 28, duration: 0.45, ease: 'power2.out' }, 0.35)
      }
      to(qa('.face-big .chev'), { autoAlpha: 0, duration: 0.35 }, 0)
      from([...tabsEl], { autoAlpha: 0, duration: 0.45, ease: 'none' }, 0.4)
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
          from(t, qa('.ws-seg'), { scaleY: 0, transformOrigin: 'top', duration: 0.6, ease: 'power2.out', stagger: 0.04 }, 0.35)
          from(t, qa('.cal-line .cal-band'), { scaleX: 0, transformOrigin: 'left center', duration: 0.6, ease: 'power2.out', stagger: 0.1 }, 0.6)
          from(t, qa('.cal-line .dl-label, .cal-line .dl-now'), { autoAlpha: 0, duration: 0.4 }, 1.0)
          from(t, qa('.cal-list li'), { autoAlpha: 0, y: 10, duration: 0.5, ease: 'power2.out', stagger: 0.08 }, 0.85)
          from(t, qa('.week'), { autoAlpha: 0, y: 36, duration: 1, ease: 'power2.out' }, 0.45)
          from(t, qa('.week .band'), { autoAlpha: 0, y: 10, duration: 0.6, ease: 'power2.out', stagger: 0.05 }, 0.9)
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

  // A session starting or ending changes the face, so the page goes back to it.
  useEffect(() => {
    if (window.scrollY > 0) scrollToY(0)
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
        {motion && session && mobile && <div className="home-sheet">{doneButton}</div>}
        {compactLanding ? today : events && <section className="today-line"><DayLine events={visible} now={now} nextId={next?.id} /></section>}
      </section>
      <section ref={ground} className="today-ground">
        {!mobile && <DebriefFold />}
        {!mobile && day && <SoFar rows={day.history} />}
        <section id="calendar-slot">
          <CalendarSection
            notify={notify}
            refresh={refresh}
            onChanged={onChanged}
            day={day}
            onStartBlock={start}
          />
        </section>
      </section>
    </div>
  )
}
