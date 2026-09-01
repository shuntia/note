# Daylight Step 6 — The Now Screen (focus mode) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the full-bleed Now screen — a 240° gauge, the soft-drift elapsed counter, the breathing center stack, and a pull-up sheet — as the resting face of an active focus session.

**Architecture:** A `FocusSession` record in `localStorage` (`note.nowSession`) is the single source of truth for "a session is running". The shell reads it at mount and renders `<Now>` *instead of* the sidebar/capture/tabs chrome whenever it is non-null, so the Now screen is full-bleed, owns its own keys, and is the resting face after reload. The counter is an imperative island: React renders one empty `<div>` and a `useEffect` mutates cells directly, so a tick never re-renders the digits.

**Tech Stack:** React 19 + TypeScript + Vite, plain CSS with the existing oklch token set, no router, no new dependencies.

**Spec:** `docs/superpowers/plans/2026-09-01-daylight-ui-spec.md` (step 6) and `docs/superpowers/plans/2026-08-31-now-counter-spec.md` (implemented verbatim). Visual truth: `docs/superpowers/mockups/daylight/now-screen-dark.html`; digit transition **D** from `tick-candidates.html`.

## Global Constraints

- Existing tokens only (`--sun`, `--sun-ink`, `--moss`, `--clay`, `--mist`, `--quiet`, `--ink`, `--dawn`, `--card`, `--serif`, `--sans`). No new global tokens — that is step 10.
- **Never introduce red.** Overrun turns nothing red, fires no notification, no modal, no sound.
- Fonts: build against `var(--serif)` (currently `Georgia, 'Iowan Old Style', 'Times New Roman', serif`). No `@font-face`, no downloads — Fraunces arrives in step 10.
- Copy verbatim: `Done`, `Next · <event>, <HH:MM>`, `Break · End session`, `Take a break`, `Back to it`, `End session`, `Switch the number`, `paused`, `step <k> of <n> · <child name>`.
- Escape-shaped actions live ONLY in the pull-up sheet; nothing on the main surface defers.
- Accessibility floor: visible keyboard focus everywhere; hit targets ≥ 40×40 px; text contrast ≥ 4.5:1; `prefers-reduced-motion: reduce` disables all decorative animation (entry beats render complete instantly, breathing/blur-fade off, completion crossfade instant).
- `overscroll-behavior: none` on this view.
- localStorage keys follow the `note.*` convention: `note.nowSession`, `note.nowCounter`.
- No Rust changes.

---

## File Structure

| File | Responsibility |
|---|---|
| `web/src/session.ts` (new) | `FocusSession` type, its localStorage read/write, elapsed math, `note.nowCounter` mode read/write. No React. |
| `web/src/nowcounter.tsx` (new) | `NowCounter` — the counter spec's §6 component, extended with `pausedAt`. |
| `web/src/views/Now.tsx` (new) | The screen: header, gauge, center stack, Done, Next, pull-up sheet, idle/pin behavior, end-of-session writes. |
| `web/src/app.tsx` | Owns `session` state; renders `<Now>` in place of the shell; `openNow` on `ViewProps`. |
| `web/src/views/Tasks.tsx` | `startFocus` body → navigation only. |
| `web/src/views/Today.tsx` | Now card title becomes the untimed-session entry point. |
| `web/src/views/Settings.tsx` | Appearance gains `Focus timer shows: Elapsed / Remaining`. |
| `web/src/api.ts` | `TaskPatch` gains `notes?: string`. |
| `web/src/styles.css` | Now-screen and counter CSS (counter block copied verbatim from the counter spec §4). |

Server verification (read-only): `server/src/tasks.rs:83-96` declares `TaskPatch.notes: Option<String>` and `:491` writes `notes = COALESCE(?4, notes)`; `server/src/api.rs:146-158` routes `PATCH /api/tasks/:id` straight into it. **Notes are patchable** — §6.6's "elapsed time noted via the agent-visible task notes" is implementable with no Rust change. `COALESCE` *replaces* rather than appends, so the client must send `previous + "\n" + line`.

---

## Task 1: Session store and counter mode

**Files:**
- Create: `web/src/session.ts`
- Modify: `web/src/api.ts` (`TaskPatch`)

**Interfaces:**
- Produces:
  ```ts
  export type FocusSession = {
    taskId: number | null      // the task the session works on (a step, when split)
    eventId: number | null     // set for an untimed session started from Today's Now card
    title: string              // parent title for a step, else the task/event name
    notes: string              // the target's notes at start, so End can append
    stepIndex: number | null   // k in `step k of n`
    stepCount: number | null
    stepName: string | null
    durationSec: number | null // null = untimed
    startedAt: number          // epoch ms
    pausedAt: number | null    // epoch ms while paused
    pausedMs: number           // accumulated paused time
  }
  export function readSession(): FocusSession | null
  export function writeSession(s: FocusSession | null): void
  export function elapsedSec(s: FocusSession): number
  export function effectiveStart(s: FocusSession): number   // startedAt + pausedMs
  export type CounterMode = 'elapsed' | 'remaining'
  export function readCounterMode(): CounterMode
  export function writeCounterMode(m: CounterMode): void
  ```

- [ ] **Step 1: Write `web/src/session.ts`**

```ts
const SESSION_KEY = 'note.nowSession'
const MODE_KEY = 'note.nowCounter'

export type FocusSession = {
  taskId: number | null
  eventId: number | null
  title: string
  notes: string
  stepIndex: number | null
  stepCount: number | null
  stepName: string | null
  durationSec: number | null
  startedAt: number
  pausedAt: number | null
  pausedMs: number
}

export type CounterMode = 'elapsed' | 'remaining'

function isSession(v: unknown): v is FocusSession {
  if (typeof v !== 'object' || v === null) return false
  const s = v as Record<string, unknown>
  return typeof s.title === 'string' && typeof s.startedAt === 'number' && typeof s.pausedMs === 'number'
}

export function readSession(): FocusSession | null {
  try {
    const raw = localStorage.getItem(SESSION_KEY)
    if (!raw) return null
    const parsed: unknown = JSON.parse(raw)
    return isSession(parsed) ? parsed : null
  } catch {
    return null
  }
}

export function writeSession(s: FocusSession | null) {
  try {
    if (s) localStorage.setItem(SESSION_KEY, JSON.stringify(s))
    else localStorage.removeItem(SESSION_KEY)
  } catch {
    // storage blocked; the session still holds for this tab
  }
}

// Paused time never counts, so a paused session reads the same second forever.
export function effectiveStart(s: FocusSession): number {
  return s.startedAt + s.pausedMs
}

export function elapsedSec(s: FocusSession): number {
  const at = s.pausedAt ?? Date.now()
  return Math.max(0, Math.floor((at - effectiveStart(s)) / 1000))
}

export function readCounterMode(): CounterMode {
  try {
    return localStorage.getItem(MODE_KEY) === 'remaining' ? 'remaining' : 'elapsed'
  } catch {
    return 'elapsed'
  }
}

export function writeCounterMode(m: CounterMode) {
  try {
    localStorage.setItem(MODE_KEY, m)
  } catch {
    // the choice still holds for this session
  }
}
```

- [ ] **Step 2: Widen `TaskPatch` in `web/src/api.ts`**

```ts
type TaskPatch = { state?: TaskState; is_now?: boolean; notes?: string }
```

- [ ] **Step 3: Typecheck**

Run: `cd web && npx tsc --noEmit`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add web/src/session.ts web/src/api.ts
git commit -m "feat: focus session store"
```

---

## Task 2: The counter component and its CSS

**Files:**
- Create: `web/src/nowcounter.tsx`
- Modify: `web/src/styles.css` (append the counter block)

**Interfaces:**
- Consumes: nothing.
- Produces: `NowCounter({ startedAt, durationSec, mode, pausedAt })`, class `now-counter`.

The counter spec's §6 reference implementation is copied verbatim; the ONLY addition is the optional `pausedAt` prop, which substitutes for `Date.now()` inside `value()` and joins the effect deps. Everything the spec's checklist tests (250 ms poll, second-derived render, `animate: false` on mount/catch-up, `key={mode}`, the `void offsetWidth` reflow, one-ghost rule) is unchanged.

- [ ] **Step 1: Write `web/src/nowcounter.tsx`**

```tsx
import { useEffect, useRef } from 'react'

function fmt(totalSeconds: number): string {
  const m = Math.floor(totalSeconds / 60)
  const s = totalSeconds % 60
  return `${m}:${String(s).padStart(2, '0')}`
}

function makeCell(c: string): HTMLSpanElement {
  const cell = document.createElement('span')
  cell.className = c === ':' ? 'cell colon' : 'cell'
  const glyph = document.createElement('span')
  glyph.className = 'glyph'
  glyph.textContent = c
  cell.append(glyph)
  return cell
}

function render(el: HTMLElement, text: string, animate: boolean): void {
  const cells = el.children
  if (cells.length !== text.length) {
    el.replaceChildren(...[...text].map(makeCell))
    return
  }
  ;[...text].forEach((c, i) => {
    const cell = cells[i] as HTMLElement
    const glyph = cell.querySelector('.glyph') as HTMLElement
    if (glyph.textContent === c) return
    if (animate) {
      cell.querySelector('.ghost')?.remove()
      const ghost = document.createElement('span')
      ghost.className = 'ghost'
      ghost.textContent = glyph.textContent ?? ''
      ghost.addEventListener('animationend', () => ghost.remove())
      cell.append(ghost)
    }
    glyph.textContent = c
    if (animate) {
      glyph.classList.remove('chg')
      void glyph.offsetWidth
      glyph.classList.add('chg')
    }
  })
}

// `pausedAt` freezes the clock the counter reads; `startedAt` already carries
// accumulated pause, so resuming continues from the frozen value.
export function NowCounter({
  startedAt,
  durationSec,
  mode,
  pausedAt,
}: {
  startedAt: number
  durationSec: number
  mode: 'elapsed' | 'remaining'
  pausedAt: number | null
}) {
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const el = ref.current
    if (!el) return
    const value = () => {
      const elapsed = Math.max(0, Math.floor(((pausedAt ?? Date.now()) - startedAt) / 1000))
      return mode === 'remaining' ? Math.max(0, durationSec - elapsed) : elapsed
    }
    let last = -1
    const tick = (animate: boolean) => {
      const v = value()
      if (v === last) return
      last = v
      render(el, fmt(v), animate)
    }
    tick(false)
    const id = setInterval(() => tick(!document.hidden), 250)
    const onVisible = () => {
      if (!document.hidden) tick(false)
    }
    document.addEventListener('visibilitychange', onVisible)
    return () => {
      clearInterval(id)
      document.removeEventListener('visibilitychange', onVisible)
    }
  }, [startedAt, durationSec, mode, pausedAt])

  return <div className="now-counter" aria-live="off" key={mode} ref={ref} />
}
```

- [ ] **Step 2: Append the counter CSS to `web/src/styles.css`, verbatim from counter spec §4**

The only edit to the spec's block is the font family, which resolves through the existing token so step 10 can swap Fraunces in without touching this layout:

```css
/* Now-screen counter — 2026-08-31-now-counter-spec.md §4 */
.now-counter {
  font-family: var(--serif);
  font-weight: 420;
  font-variant-numeric: tabular-nums;
  line-height: 1;
}
/* pseudo-monospace: fixed-width cells so layout never shifts when digits change */
.now-counter .cell { position: relative; display: inline-block; width: .6em; text-align: center; }
.now-counter .cell.colon { width: .32em; }
.now-counter .glyph { display: inline-block; }

@keyframes nc-drift-in {
  0%   { transform: translateY(.38em); opacity: 0; filter: blur(5px); }
  100% { transform: translateY(0);     opacity: 1; filter: blur(0); }
}
@keyframes nc-drift-out {
  0%   { transform: translateY(0);      opacity: 1; filter: blur(0); }
  100% { transform: translateY(-.38em); opacity: 0; filter: blur(5px); }
}
.now-counter .glyph.chg { animation: nc-drift-in .7s cubic-bezier(.3,.6,.25,1); }
.now-counter .ghost {
  position: absolute;
  inset: 0;
  pointer-events: none;
  animation: nc-drift-out .7s cubic-bezier(.3,.6,.25,1) forwards;
}
@media (prefers-reduced-motion: reduce) {
  .now-counter .glyph.chg { animation: none; }
  .now-counter .ghost { display: none; }
}
```

- [ ] **Step 3: Typecheck and commit**

```bash
cd web && npx tsc --noEmit
git add web/src/nowcounter.tsx web/src/styles.css
git commit -m "feat: now-screen soft-drift counter"
```

---

## Task 3: The Now screen

**Files:**
- Create: `web/src/views/Now.tsx`
- Modify: `web/src/styles.css` (screen block)

**Interfaces:**
- Consumes: `FocusSession`, `readCounterMode`/`writeCounterMode`, `NowCounter`, `api.planToday`, `api.patchTask`, `api.eventAction`, `eventLabel`, `ToastAction`.
- Produces:
  ```tsx
  export function Now({ session, setSession, notify, onChanged, onLeave }: {
    session: FocusSession
    setSession: (s: FocusSession | null) => void
    notify: (msg: string, action?: ToastAction) => void
    onChanged: () => void
    onLeave: () => void
  })
  ```

**Geometry (from the mockup, which wins on look):** `viewBox="0 0 320 320"`, `r=150`, `stroke-width: 11`, `stroke-linecap: round`, wrapper 380×380, group `transform="rotate(150 160 160)"`. Circumference `2π·150 = 942.48`; the 240° sweep is `628.32`. Progress length = `628.32 × min(1, elapsed/duration)`.

**Entry beats:** track `animation: now-draw .6s cubic-bezier(.3,.7,.3,1) .1s forwards` → completes at **700 ms**. Arc `animation: now-draw 1.2s cubic-bezier(.22,.9,.35,1) .5s forwards` → completes at **1700 ms**. The track therefore visibly completes a full second before the arc finishes.

- [ ] **Step 1: Write `web/src/views/Now.tsx`**

```tsx
import { useCallback, useEffect, useRef, useState } from 'react'
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

const ARC = 628.32
const IDLE_MS = 30_000
const CROSSFADE_MS = 500
const SWIPE_PX = 40

const reduced = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches

function clock(at: Date): string {
  const day = at.toLocaleDateString(undefined, { weekday: 'long' })
  return `${day} · ${String(at.getHours()).padStart(2, '0')}:${String(at.getMinutes()).padStart(2, '0')}`
}

function wallMinutes(wall: string): number {
  const [h, m] = wall.split(':')
  return Number(h) * 60 + Number(m)
}

// The next thing still ahead, so the line under Done is a preview and never a nag.
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

// Notes are the agent's window on the session; COALESCE replaces the column, so
// the previous text has to travel back out with the new line.
function withElapsedNote(previous: string, elapsed: number): string {
  const line = `${new Date().toISOString().slice(0, 10)} · focused ${Math.max(1, Math.round(elapsed / 60))} min`
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
    api.planToday().then(setEvents).catch(() => setEvents([]))
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

  // The arc grows on its own clock: the counter owns the digits, this owns the sweep.
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
      const patch = done
        ? { state: 'done' as const, notes: withElapsedNote(session.notes, elapsed) }
        : { notes: withElapsedNote(session.notes, elapsed) }
      api
        .patchTask(session.taskId, patch)
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

  const onTouchStart = (e: React.TouchEvent) => {
    touchY.current = e.touches[0]?.clientY ?? null
  }
  const onTouchEnd = (e: React.TouchEvent) => {
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
  const overrun =
    session.durationSec !== null && elapsedSec(session) >= session.durationSec

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
        <div
          className="now-ring"
          role="group"
          aria-label={spoken(elapsedSec(session), session.durationSec)}
        >
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
                <circle
                  className="now-arc"
                  cx="160"
                  cy="160"
                  r="150"
                  style={{ '--arc': `${arcLen}` } as React.CSSProperties}
                />
              )}
              <circle
                className="now-moss"
                cx="160"
                cy="160"
                r="150"
                style={
                  {
                    '--arc': `${session.durationSec === null ? ARC : arcLen}`,
                  } as React.CSSProperties
                }
              />
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
```

- [ ] **Step 2: Append the screen CSS to `web/src/styles.css`**

`--now-faint` is a locally scoped value, not a global token; it measures 4.81:1 (light) and 6.49:1 (dark) against `--dawn` — verified in Task 6.

```css
/* Now screen (focus mode) */
.now-screen {
  position: fixed;
  inset: 0;
  z-index: 20;
  display: flex;
  flex-direction: column;
  align-items: center;
  overflow: hidden;
  overscroll-behavior: none;
  touch-action: none;
  background:
    radial-gradient(120% 90% at 50% 118%, color-mix(in srgb, var(--sun) 6%, transparent), transparent 60%),
    var(--dawn);
  color: var(--ink);
  /* one step quieter than --quiet, and no quieter: 4.8:1 is the floor it clears */
  --now-faint: color-mix(in srgb, var(--quiet) 92%, var(--dawn));
}
.now-top {
  width: 100%;
  max-width: 67.5rem;
  display: flex;
  align-items: baseline;
  padding: 1.5rem 2.125rem 0;
  color: var(--now-faint);
  font-size: 0.875rem;
}
.now-brand { font-family: var(--serif); font-weight: 600; font-size: 0.9375rem; }
.now-clock { margin-left: auto; font-variant-numeric: tabular-nums; }

.now-stage {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 1.625rem;
}
.now-ring { position: relative; width: 380px; height: 380px; max-width: 88vw; max-height: 88vw; }
.now-gauge { width: 100%; height: 100%; }
.now-track, .now-arc, .now-moss { fill: none; stroke-width: 11; stroke-linecap: round; }
.now-track {
  stroke: var(--mist);
  stroke-dasharray: 628.32 942.48;
  stroke-dashoffset: 628.32;
  animation: now-draw .6s cubic-bezier(.3,.7,.3,1) .1s forwards;
}
.now-arc {
  stroke: url(#now-sungrad);
  stroke-dasharray: var(--arc) 942.48;
  stroke-dashoffset: var(--arc);
  animation: now-draw 1.2s cubic-bezier(.22,.9,.35,1) .5s forwards;
  filter: drop-shadow(0 0 7px color-mix(in srgb, var(--sun) 35%, transparent));
  transition: opacity 300ms ease;
}
.now-screen.paused .now-arc { opacity: .5; }
.now-moss {
  stroke: var(--moss);
  stroke-dasharray: var(--arc) 942.48;
  stroke-dashoffset: 0;
  opacity: 0;
}
.now-screen.finishing .now-moss { animation: now-moss .5s ease forwards; }
@keyframes now-draw { to { stroke-dashoffset: 0; } }
@keyframes now-moss { to { opacity: 1; } }

.now-center {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  text-align: center;
  gap: 0.625rem;
  padding: 0 3.25rem;
  transition: filter .8s ease, opacity .8s ease;
}
.now-screen.ambient .now-center { filter: blur(12px); opacity: 0; }
.now-counter { font-size: 82px; letter-spacing: -.015em; }
.now-center.over .now-counter { color: var(--sun-ink); }
.now-denom {
  display: flex;
  align-items: center;
  gap: 0.5625rem;
  width: 150px;
  color: var(--now-faint);
  font-size: 0.78125rem;
}
.now-denom::before, .now-denom::after { content: ""; flex: 1; border-top: 1.5px dashed var(--mist); }
.now-task { color: var(--quiet); font-size: 0.96875rem; text-wrap: balance; margin-top: 0.375rem; }
.now-step { color: var(--now-faint); font-size: 0.78125rem; }

.now-done {
  font-family: var(--sans);
  font-size: 0.875rem;
  color: var(--quiet);
  background: none;
  border: 1px solid var(--mist);
  border-radius: 999px;
  min-height: 2.5rem;
  padding: 0.5rem 1.625rem;
  cursor: pointer;
}
.now-done:hover { border-color: var(--quiet); color: var(--ink); }
.now-done:focus-visible { outline: 2px solid var(--ring); outline-offset: 2px; }
.now-next { color: var(--now-faint); font-size: 0.75rem; margin-top: 0.125rem; }

.now-sheet {
  position: fixed;
  left: 50%;
  bottom: 0;
  transform: translateX(-50%);
  width: 250px;
  background: var(--card);
  border: 1px solid var(--mist);
  border-bottom: none;
  border-radius: 16px 16px 0 0;
  padding-bottom: env(safe-area-inset-bottom);
}
.now-lip {
  display: block;
  width: 100%;
  min-height: 2.5rem;
  padding: 0.5625rem 0 0.375rem;
  background: none;
  border: none;
  border-radius: 16px 16px 0 0;
  color: inherit;
  font: inherit;
  cursor: pointer;
}
.now-lip:focus-visible { outline: 2px solid var(--ring); outline-offset: -2px; }
.now-grab { display: block; width: 38px; height: 4px; border-radius: 2px; background: var(--mist); margin: 0 auto 6px; }
.now-hint { color: var(--now-faint); font-size: 0.75rem; }
.now-sheet-body { display: flex; flex-direction: column; padding: 0 0.5rem 0.5rem; }
.now-sheet-item {
  min-height: 2.5rem;
  padding: 0.5rem 0.75rem;
  background: none;
  border: none;
  border-top: 1px solid var(--mist);
  color: var(--quiet);
  font: inherit;
  font-size: 0.9rem;
  text-align: left;
  cursor: pointer;
}
.now-sheet-item:hover { color: var(--ink); }
.now-sheet-item:focus-visible { outline: 2px solid var(--ring); outline-offset: -2px; }

@media (prefers-reduced-motion: reduce) {
  .now-track, .now-arc { animation: none; stroke-dashoffset: 0; }
  .now-center { transition: none; }
  .now-screen.ambient .now-center { filter: none; }
  .now-screen.finishing .now-moss { animation: none; opacity: 1; }
}
```

- [ ] **Step 3: Typecheck and commit**

```bash
cd web && npx tsc --noEmit
git add web/src/views/Now.tsx web/src/styles.css
git commit -m "feat: now screen focus surface"
```

---

## Task 4: Shell wiring and entry points

**Files:**
- Modify: `web/src/app.tsx`, `web/src/views/Tasks.tsx`, `web/src/views/Today.tsx`, `web/src/styles.css`

**Interfaces:**
- Produces: `ViewProps.openNow(session: FocusSession): void`.

When `session` is non-null the shell renders `<Now>` and the toast *only* — the sidebar, capture bar, and tab bar do not mount, so the global `n` binding cannot fight the screen's `Esc`/`ArrowUp`.

- [ ] **Step 1: `web/src/app.tsx` — session state**

```tsx
const [session, setSession] = useState<FocusSession | null>(readSession)

const openNow = useCallback((s: FocusSession) => {
  setSession(s)
  writeSession(s)
}, [])

const changeSession = useCallback((s: FocusSession | null) => {
  setSession(s)
  writeSession(s)
}, [])
```

Add `openNow` to `ViewProps` and to the `views` object. Before the shell's `return`:

```tsx
if (session) {
  return (
    <>
      <Now
        session={session}
        setSession={changeSession}
        notify={notify}
        onChanged={onChanged}
        onLeave={() => setTab('today')}
      />
      {toastNode}
    </>
  )
}
```

Lift the existing toast JSX into a `toastNode` const so both branches render it.

- [ ] **Step 2: `web/src/views/Tasks.tsx` — `startFocus` becomes navigation**

```tsx
const startFocus = (node: TaskNode) => {
  const target = focusTarget(node)
  const index = node.children.findIndex((c) => c.id === target.id)
  openNow({
    taskId: target.id,
    eventId: null,
    title: node.title,
    notes: target.notes,
    stepIndex: index === -1 ? null : index + 1,
    stepCount: index === -1 ? null : node.children.length,
    stepName: index === -1 ? null : target.title,
    durationSec: target.duration_min === null ? null : round5(target.duration_min) * 60,
    startedAt: Date.now(),
    pausedAt: null,
    pausedMs: 0,
  })
}
```

Destructure `openNow` from `ViewProps` in the `Tasks` signature. Delete the old two-line body and its comment; nothing else in this file moves.

- [ ] **Step 3: `web/src/views/Today.tsx` — the Now card title starts an untimed session**

`NowCard` takes an `openNow` prop threaded from `Today`'s `ViewProps` through `spine`. The `h2` keeps `nowcard-title` (its typography) and wraps a button that inherits it:

```tsx
<button
  className="nowcard-open"
  onClick={() =>
    openNow({
      taskId: null,
      eventId: ev.id,
      title: eventLabel(ev.kind),
      notes: '',
      stepIndex: null,
      stepCount: null,
      stepName: null,
      durationSec: null,
      startedAt: Date.now(),
      pausedAt: null,
      pausedMs: 0,
    })
  }
>
  {eventLabel(ev.kind)}
</button>
```

- [ ] **Step 4: `web/src/styles.css` — the title button keeps its typography**

```css
.nowcard-open {
  display: inline-block;
  max-width: 100%;
  min-height: 2.5rem;
  padding: 0;
  background: none;
  border: none;
  color: inherit;
  font: inherit;
  text-align: left;
  cursor: pointer;
}
.nowcard-open:focus-visible { outline: 2px solid var(--ring); outline-offset: 3px; }
```

- [ ] **Step 5: Typecheck, build, commit**

```bash
cd web && npx tsc --noEmit && npx vite build
git add web/src/app.tsx web/src/views/Tasks.tsx web/src/views/Today.tsx web/src/styles.css
git commit -m "feat: now screen entry points"
```

---

## Task 5: The elapsed/remaining setting

**Files:**
- Modify: `web/src/views/Settings.tsx`

Counter spec §7: Settings → Appearance, "Focus timer shows: Elapsed / Remaining", persisted at `note.nowCounter`, default `elapsed`. The sheet's `Switch the number` writes the same key.

- [ ] **Step 1: Add the row to `AppearanceSection`**

```tsx
const COUNTERS: { id: CounterMode; label: string }[] = [
  { id: 'elapsed', label: 'Elapsed' },
  { id: 'remaining', label: 'Remaining' },
]
```

```tsx
<div className="pane-row">
  <span className="pane-label" id="counter-label">
    Focus timer shows
  </span>
  <div className="seg" role="group" aria-labelledby="counter-label">
    {COUNTERS.map((c) => (
      <button
        key={c.id}
        type="button"
        aria-pressed={counter === c.id}
        onClick={() => {
          setCounter(c.id)
          writeCounterMode(c.id)
        }}
      >
        {c.label}
      </button>
    ))}
  </div>
  <p className="pane-hint">The big number on the Now screen counts up, or counts down.</p>
</div>
```

- [ ] **Step 2: Typecheck and commit**

```bash
cd web && npx tsc --noEmit
git add web/src/views/Settings.tsx
git commit -m "feat: focus timer elapsed/remaining setting"
```

---

## Task 6: Verification

**Files:** none (evidence only).

- [ ] **Step 1: `cd web && npx tsc --noEmit && npx vite build`** — both must pass; paste real output.

- [ ] **Step 2: Drive the system `chromium` via `playwright-core` + CDP** (Playwright's bundled Chromium lacks `libgbm.so.1` on this host). Serve the built bundle with a stub API so the screen has a session, and assert:

  **Counter spec checklist (9):**
  1. At rest: `N` cells, one `.glyph` each, zero `.ghost`.
  2. MutationObserver on a minutes cell across 10 ticks → zero records.
  3. `getBoundingClientRect().width` identical before/during/after a tick.
  4. Hammer render with alternating characters every 100 ms for 3 s → no cell holds two ghosts.
  5. `9:59 → 10:00` rebuilds: cell count changes, zero ghosts, zero running animations.
  6. `prefers-reduced-motion: reduce` (CDP `Emulation.setEmulatedMedia`) → instant swap, no ghost rendered.
  7. Hide the tab >10 s (`Emulation.setPageVisibilityOverride` / real `visibilitychange`), return → snaps, `getAnimations()` empty.
  8. Toggle elapsed/remaining → number swaps with zero animations.
  9. Drift matches D: assert the keyframes resolve to `translateY(.38em)`/`blur(5px)`/`.7s`/`cubic-bezier(0.3, 0.6, 0.25, 1)` and that no horizontal offset occurs.

  **Step 6 acceptance:**
  - Entry beats: read `getAnimations()` end times — track 700 ms < arc 1700 ms.
  - Idle 30 s → `.ambient`; swipe down pins; `Done` still hit-testable and clickable while ambient.
  - Pause: arc opacity `0.5`, counter text frozen across 3 s, `paused` in the underline row, sheet reads `Back to it`, resume continues from the frozen value.
  - Overrun: sweep every computed `color`/`background-color`/`border-color`/`stroke` on the subtree; assert nothing is red (r dominant with low g/b) and no `Notification` was constructed.
  - Reduced motion: both entry beats complete at load (`stroke-dashoffset` 0, zero animations).
  - Reload with `note.nowSession` set → Now screen is the resting face and the elapsed value continues.
  - Contrast: compute `--now-faint` vs `--dawn` in both themes; require ≥ 4.5:1.
  - Hit targets: every `button` inside `.now-screen` measures ≥ 40×40 px.

- [ ] **Step 3: `cargo test --workspace`** — unchanged, 264 passing.

- [ ] **Step 4: Final commit**

```bash
git commit -m "feat(web): daylight step 6 — the Now screen"
```

---

## Self-Review

**Spec coverage:** 6.1 entry points → Task 4 (all three: Start button, Now card title, resting face via `readSession` at mount). 6.2 layout → Task 3 (whisper header with no sun disc; 380 px gauge; `Done` pill; `Next ·` line; sheet lip). 6.3 gauge → Task 3 geometry + entry beats + untimed track-only. 6.4 center stack → Task 3. 6.5 breathing/swipe/keys/`overscroll-behavior` → Task 3. 6.6 sheet → Task 3, notes write proven patchable above. 6.7 overrun → `.now-center.over` uses `--sun-ink`, arc clamps to `ARC`, no notification path exists. 6.8 completion → `.now-moss` crossfade + reduced-motion instant. Counter spec §1–§8 → Task 2 verbatim; §7 setting → Task 5; §9 → Task 6.

**Placeholders:** none — every step carries the code it asks for.

**Type consistency:** `FocusSession` fields are identical in Tasks 1, 3, and 4; `CounterMode` is used by Tasks 1, 3, and 5; `NowCounter`'s four props match its two call-sites' shape; `openNow` has one signature throughout.
