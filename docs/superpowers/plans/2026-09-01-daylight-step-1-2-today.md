# Daylight Steps 1–2: Today action hierarchy, Now line and Now card — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild the Today view so exactly one event — the current one — carries actions in a clear hierarchy inside an enlarged Now card sitting under a labelled Now line, with the debrief folded into a single row at the top.

**Architecture:** Presentation-only rewrite of `web/src/views/Today.tsx` plus its slice of `web/src/styles.css`. The spine keeps the existing `GET /api/plan/today` payload; all new copy is derived client-side from `status`, `flexibility`, `slide_window_min` and `channel`. The one shared change is widening the app-level toast to carry an action button, which the Drop undo needs.

**Tech Stack:** React 19 + TypeScript + Vite, hand-written CSS with oklch custom properties. No test runner in `web/`; verification is `npx tsc --noEmit`, `npx vite build`, and a headless-chromium screenshot pass against a mock API.

**Spec:** `docs/superpowers/plans/2026-09-01-daylight-ui-spec.md` — steps 1 and 2, folded into one plan as the spec's self-review notes permit.

## Global Constraints

- Design tokens: only what `web/src/styles.css` already defines — `--sun` `#e8871e`, `--sun-ink`, `--moss`, `--clay`, and the oklch monochrome scale (`--bg`/`--dawn`, `--surface`/`--card`, `--sunk`, `--text`/`--ink`, `--text-muted`/`--quiet`, `--border`/`--mist`, `--border-input`). **No new global tokens, no new fonts** — that is step 10. Accent = amber only; moss = done; clay = dropped. **Never introduce red** (do not use `--danger` anywhere in Today).
- Copy rules: sentence case; active voice; an action keeps its name through its whole flow; user vocabulary, never system vocabulary.
- Dropped/missed items: recorded quietly, past tense, clay at most. No alarm styling, no modal interruptions about the past.
- Every state-changing action gets feedback within 100 ms (optimistic UI) and, where destructive, an in-place Undo. Nothing is confirm-dialog-guarded if it can be undo-guarded.
- Accessibility floor: visible keyboard focus on every interactive element; `prefers-reduced-motion` disables all decorative animation; hit targets ≥ 40×40 px on touch layouts; text contrast ≥ 4.5:1.
- All assets bundled; the client makes no external requests.
- Done/Later/Drop keep their existing API semantics. This step is presentation only — no server changes.
- Out of scope, handled by later steps: the capture bar (step 3), schedule blocks and bells (step 7), the spine gradient / Fraunces / Atkinson / diamond removal (step 10).

### Server facts this plan argues from

Read out of `server/src/plan.rs`, `server/src/api.rs`, `server/src/db.rs`, `server/src/templates.rs`:

- `PlanEvent` serializes verbatim: `id`, `kind`, `wall_time`, `status`, `flexibility`, `slide_window_min`, `channel`. `web/src/types.ts` already mirrors it exactly — **no type change needed**.
- `status` ∈ `pending | fired | snoozed | done | dropped`. `flexibility` ∈ `fixed | slide | drop`. `channel` ∈ `push | voice` (`ws` is an internal delivery name, never on an event).
- `slide_window_min` is only consulted for shifts, and only when `> 0`; `0` means unbounded, not immovable.
- `POST /events/{id}/shift` returns **404 for `flexibility: fixed`**, 409 when done/dropped, 400 outside the window. So the ±15 chips belong on `flexibility !== 'fixed'` only — which is what the current code already does.
- `POST /events/{id}/snooze` works for every flexibility; 400 outside `1..=1440`; 409 when done/dropped.
- **`drop` is irreversible: there is no un-drop endpoint and `set_status` only accepts `"done"` or `"dropped"`.** The undo in step 1 therefore cannot be a compensating request — it must be a *held* request that is only sent once the undo window closes.
- `events_for()` sorts `ORDER BY wall_time`, and `wall_time` is zero-padded `HH:MM` local wall clock, so string compare is chronological.
- `kind` is free-form text. Shipped/observed values: `checkin`, `checkin_call`, `nudge`, `debrief`.

### Deliberate deviations from the mockups (behaviour and a11y outrank look)

- The mockups' `--faint` (`oklch(62%)` light) scores ≈3.5:1 on paper — under the binding 4.5:1 floor. Everywhere a mockup says `--faint` **or** `--quiet`, use `var(--text-muted)`; hierarchy comes from size and weight instead of a third grey.
- The mockups show the step-3 capture bar, step-7 blocks/bells and the step-10 spine gradient. None of those are built here.

---

## File Structure

| File | Responsibility after this plan |
|---|---|
| `web/src/app.tsx` | Modified. `notify` gains an optional action so a toast can carry an Undo button; the toast node renders it. Nothing else changes. |
| `web/src/views/Today.tsx` | Rewritten. Loads the plan and debrief; picks the current event; renders the folded debrief row, the spine (past rows, Now line, Now card, future rows), and the tomorrow line. Holds all derived copy helpers. |
| `web/src/styles.css` | The `today` block (currently lines ~377–485) is replaced with the Daylight spine/Now-line/Now-card/debrief-fold rules; the toast gains an action button rule; a `(pointer: coarse)` block raises new hit targets. |

`Today.tsx` lands around 400 lines — the same order as the existing `Talk.tsx` (15 KB) and `Settings.tsx` (21 KB), so it follows the repo's flat `views/` convention rather than introducing a subdirectory.

---

### Task 1: Toast actions

**Files:**
- Modify: `web/src/app.tsx:14-18` (`ViewProps`), `:32-40` (toast state and `notify`), `:116` (toast node)
- Modify: `web/src/styles.css:1397-1413` (`.toast`)
- Test: none — no frontend test runner exists; verified by `tsc` and by Task 5's screenshots.

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```ts
  export type ToastAction = { label: string; run: () => void }
  export type ViewProps = {
    notify: (msg: string, action?: ToastAction) => void
    refresh: number
    onChanged: () => void
  }
  ```
  Existing single-argument `notify(msg)` calls in `Tasks.tsx`, `Memory.tsx`, `Settings.tsx` and `app.tsx` keep working unchanged.

- [ ] **Step 1: Widen the toast state and `notify` in `web/src/app.tsx`**

Replace the `ViewProps` type block:

```tsx
export type ToastAction = { label: string; run: () => void }

// `refresh` is a counter views key on or depend on to refetch; `onChanged` bumps it.
export type ViewProps = {
  notify: (msg: string, action?: ToastAction) => void
  refresh: number
  onChanged: () => void
}
```

Replace the toast state and `notify`:

```tsx
  const [toast, setToast] = useState<{ msg: string; action?: ToastAction } | null>(null)

  const toastTimer = useRef(0)
  // An undo toast has to outlast the window the action holds itself open for.
  const notify = useCallback((msg: string, action?: ToastAction) => {
    setToast({ msg, action })
    window.clearTimeout(toastTimer.current)
    toastTimer.current = window.setTimeout(() => setToast(null), action ? 5000 : 4000)
  }, [])
```

- [ ] **Step 2: Render the action button**

Replace the toast node at the end of `App`:

```tsx
      {toast && (
        <div className="toast" role="status">
          <span className="toast-msg">{toast.msg}</span>
          {toast.action && (
            <button
              className="toast-action"
              onClick={() => {
                toast.action?.run()
                setToast(null)
              }}
            >
              {toast.action.label}
            </button>
          )}
        </div>
      )}
```

- [ ] **Step 3: Style it in `web/src/styles.css`**

Replace the `.toast` rule with:

```css
/* toast */
.toast {
  position: fixed;
  bottom: 1.25rem;
  left: 50%;
  transform: translateX(-50%);
  display: flex;
  align-items: center;
  gap: 0.9rem;
  background: var(--text);
  color: var(--bg);
  padding: 0.6rem 0.7rem 0.6rem 1rem;
  border-radius: var(--radius-lg);
  max-width: 24rem;
  z-index: 30;
  box-shadow: 0 8px 24px color-mix(in srgb, var(--text) 20%, transparent);
}
.toast-msg { min-width: 0; }
.toast-action {
  flex: none;
  background: none;
  border: 1px solid color-mix(in srgb, var(--bg) 45%, transparent);
  border-radius: 999px;
  padding: 0.25rem 0.8rem;
  color: inherit;
  font: inherit;
  font-size: 0.85rem;
  font-weight: 600;
  cursor: pointer;
}
.toast-action:hover { background: color-mix(in srgb, var(--bg) 16%, transparent); }
.toast-action:focus-visible { outline: 2px solid var(--bg); outline-offset: 2px; }
@media (max-width: 767.98px) {
  .toast { bottom: calc(4.1rem + env(safe-area-inset-bottom)); }
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cd web && npx tsc --noEmit`
Expected: clean exit, no output.

- [ ] **Step 5: Commit**

```bash
git add web/src/app.tsx web/src/styles.css
git commit -m "feat: toasts can carry an action button"
```

---

### Task 2: Today view rewrite

**Files:**
- Rewrite: `web/src/views/Today.tsx` (whole file)
- Test: none available; verified by Tasks 4 and 5.

**Interfaces:**
- Consumes: `ViewProps` / `ToastAction` from Task 1; `api.planToday`, `api.debrief`, `api.eventAction`, `api.snooze`, `api.shift` from `web/src/api.ts`; `PlanEvent` and `Debrief` from `web/src/types.ts` (unchanged).
- Produces: `export function Today({ notify }: ViewProps)`. All other symbols stay module-private. Class names Task 3 styles: `today`, `debrief-fold`, `debrief-lead`, `debrief-chev`, `debrief-note`, `spine`, `ev`, `ev-time`, `ev-dot`, `ev-row`, `ev-name`, `ev-check`, `ev-tag`, `now-line`, `now-rule`, `now-dot`, `now-label`, `nowcard`, `nowcard-eyebrow`, `nowcard-title`, `nowcard-meta`, `nowcard-actions`, `btn-primary`, `btn-outline`, `btn-chip`, `actions-spacer`, `ev-more`, `ev-more-wrap`, `ev-menu`, `ev-menu-item`, `today-clear`, `today-tomorrow`.

- [ ] **Step 1: Replace `web/src/views/Today.tsx` in full**

```tsx
import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
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

function label(kind: string): string {
  if (kind === 'debrief') return 'Morning debrief'
  const words = kind.replaceAll('_', ' ').replace('checkin', 'check-in').trim()
  return words.charAt(0).toUpperCase() + words.slice(1)
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
    notify(`Dropped "${label(ev.kind)}" — moved off today`, {
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
      rows.push(
        <NowCard key={ev.id} ev={ev} now={now} act={act} drop={drop} pending={pending} />,
      )
      return
    }
    rows.push(<EventRow key={ev.id} ev={ev} above={current === -1 || i < current} />)
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

function EventRow({ ev, above }: { ev: PlanEvent; above: boolean }) {
  const state = ev.status === 'done' ? 'done' : ev.status === 'dropped' ? 'dropped' : ''
  return (
    <li className={`ev ${state} ${above ? 'above' : ''}`}>
      <span className="ev-time">{ev.wall_time}</span>
      <span className="ev-dot" aria-hidden="true" />
      <div className="ev-row">
        {state === 'done' && (
          <span className="ev-check" aria-hidden="true">
            ✓
          </span>
        )}
        <span className="ev-name">{label(ev.kind)}</span>
        <span className="ev-tag">{state === 'dropped' ? 'dropped' : state === 'done' ? '' : flexTag(ev)}</span>
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
        <Overflow onDrop={() => drop(ev)} disabled={pending} />
        <div className="nowcard-eyebrow">{eyebrow(ev, now)}</div>
        <h2 className="nowcard-title">{label(ev.kind)}</h2>
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

function Overflow({ onDrop, disabled }: { onDrop: () => void; disabled: boolean }) {
  const [open, setOpen] = useState(false)
  const wrap = useRef<HTMLDivElement>(null)
  const trigger = useRef<HTMLButtonElement>(null)
  const item = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    if (!open) return
    item.current?.focus()
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return
      setOpen(false)
      trigger.current?.focus()
    }
    const onDown = (e: MouseEvent) => {
      if (!wrap.current?.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('keydown', onKey)
    document.addEventListener('mousedown', onDown)
    return () => {
      document.removeEventListener('keydown', onKey)
      document.removeEventListener('mousedown', onDown)
    }
  }, [open])

  return (
    <div className="ev-more-wrap" ref={wrap}>
      <button
        className="ev-more"
        ref={trigger}
        aria-label="More actions"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        ⋯
      </button>
      {open && (
        <div className="ev-menu" role="menu">
          <button
            className="ev-menu-item"
            role="menuitem"
            ref={item}
            disabled={disabled}
            onBlur={() => setOpen(false)}
            onClick={() => {
              setOpen(false)
              onDrop()
            }}
          >
            Drop
          </button>
        </div>
      )}
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

function firstSentence(text: string): { lead: string; more: boolean } {
  const trimmed = text.trim()
  const match = /^[\s\S]*?[.!?](?=\s|$)/.exec(trimmed)
  const lead = match ? match[0] : trimmed
  return { lead, more: lead.length < trimmed.length }
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
  if (debrief === null) return <p className="debrief-note muted">No letter yet — it arrives overnight.</p>
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

  const { lead, more } = firstSentence(debrief.content)
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
          <b>This morning:</b> {folded ? `${lead}${more ? '…' : ''}` : ''}
        </span>
        <span className="debrief-chev" aria-hidden="true">
          {folded ? '▾' : '▴'}
        </span>
      </button>
      {!folded && <div className="letter">{debrief.content}</div>}
    </section>
  )
}
```

- [ ] **Step 2: Verify it type-checks**

Run: `cd web && npx tsc --noEmit`
Expected: clean exit. (It will still look wrong in the browser until Task 3 lands — that is expected.)

- [ ] **Step 3: Commit**

```bash
git add web/src/views/Today.tsx
git commit -m "feat: Today renders a Now card with a single-primary action row"
```

---

### Task 3: Today styles

**Files:**
- Modify: `web/src/styles.css` — replace everything from the `/* today — the plan spine … */` comment through the end of the `.event-actions` block (currently lines 377–485, ending just before `/* tasks */`).
- Modify: `web/src/styles.css:543-545` — the `prefers-reduced-motion` block that lists `.event-card`.

**Interfaces:**
- Consumes: the class names Task 2 produced.
- Produces: no new global custom properties. Uses only `--bg`, `--surface`, `--sunk`, `--text`, `--text-muted`, `--border`, `--border-input`, `--sun`, `--sun-ink`, `--moss`, `--clay`, `--serif`, `--radius`, `--radius-lg`, `--radius-xl`.

- [ ] **Step 1: Replace the today block**

```css
/* today — one reading column: the morning letter folded at the top, then the plan
   spine with the Now line cutting across it and the current event raised into a card.
   The mockups' third grey fails the 4.5:1 floor, so quiet and faint both resolve to
   --text-muted and the hierarchy is carried by size and weight. */
.today { max-width: 45rem; }

.debrief-row { margin-bottom: 1.6rem; }
.debrief-fold {
  display: flex;
  align-items: center;
  gap: 0.6rem;
  width: 100%;
  text-align: left;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  padding: 0.7rem 1rem;
  color: var(--text-muted);
  font: inherit;
  font-size: 0.95rem;
  cursor: pointer;
  transition: border-color 120ms ease;
}
.debrief-fold:hover { border-color: var(--border-input); }
.debrief-lead {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.debrief-lead b { color: var(--text); font-weight: 700; }
.debrief-chev { margin-left: auto; }
.debrief-row .letter { margin-top: 0.9rem; padding: 0 0.25rem; font-size: 1rem; }
.debrief-note { margin: 0 0 1.6rem; font-size: 0.9rem; }

/* the time spine: hours in the left gutter, a rule down the middle of it */
.spine {
  list-style: none;
  margin: 0;
  padding: 0 0 0 5.4rem;
  position: relative;
}
.spine::before {
  content: '';
  position: absolute;
  left: 4rem;
  top: 0.4rem;
  bottom: 0.4rem;
  width: 2px;
  border-radius: 2px;
  background: var(--border);
}

.ev { position: relative; margin: 0 0 0.9rem; }
.ev-time {
  position: absolute;
  left: -5.4rem;
  top: 0.1rem;
  width: 3.25rem;
  text-align: right;
  font-variant-numeric: tabular-nums;
  font-size: 0.8rem;
  color: var(--text-muted);
}
.ev-dot {
  position: absolute;
  left: -1.5rem;
  top: 0.45rem;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--border-input);
  border: 2px solid var(--bg);
}
.ev.done .ev-dot { background: var(--moss); }
.ev.dropped .ev-dot { background: transparent; border-color: var(--border-input); }

.ev-row {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 0.55rem;
  color: var(--text-muted);
  font-size: 0.95rem;
}
.ev.done .ev-name {
  text-decoration: line-through;
  text-decoration-color: color-mix(in srgb, var(--moss) 55%, transparent);
}
.ev-check { color: var(--moss); font-size: 0.85rem; }
.ev-tag:empty { display: none; }
.ev-tag {
  font-size: 0.72rem;
  border: 1px solid var(--border);
  border-radius: 999px;
  padding: 0.05rem 0.5rem;
}
.ev.dropped .ev-tag {
  color: var(--clay);
  border-color: color-mix(in srgb, var(--clay) 35%, var(--border));
}

/* the Now line: dashed across the spine, a sun disc on its axis, the minute on it */
.now-line {
  position: relative;
  height: 0;
  margin: 1.5rem 0 2rem -5.4rem;
}
.now-rule {
  position: absolute;
  left: 0;
  right: 0;
  top: 0;
  border-top: 1px dashed color-mix(in srgb, var(--sun) 55%, var(--border));
}
.now-dot {
  position: absolute;
  left: 3.7rem;
  top: -6px;
  width: 12px;
  height: 12px;
  border-radius: 50%;
  background: var(--sun);
  box-shadow: 0 0 0 4px color-mix(in srgb, var(--sun) 22%, var(--bg));
}
.now-label {
  position: absolute;
  left: 5.15rem;
  top: -0.62rem;
  padding: 0 0.5rem;
  background: var(--bg);
  color: var(--sun-ink);
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.09em;
  font-variant-numeric: tabular-nums;
}

.ev.now { margin-bottom: 1.6rem; }
.ev.now .ev-time { color: var(--sun-ink); font-weight: 700; }
.nowcard {
  position: relative;
  background: var(--surface);
  border: 1px solid color-mix(in srgb, var(--sun) 38%, var(--border));
  border-radius: var(--radius-xl);
  padding: 1.25rem 1.35rem 1.15rem;
  margin-left: -0.9rem;
  box-shadow: 0 10px 28px -18px color-mix(in srgb, var(--sun) 45%, transparent);
}
.nowcard-eyebrow {
  color: var(--sun-ink);
  font-size: 0.75rem;
  font-weight: 700;
  letter-spacing: 0.1em;
  margin-bottom: 0.35rem;
}
.nowcard-title {
  margin: 0 0 0.2rem;
  font-family: var(--serif);
  font-weight: 500;
  font-size: 1.85rem;
  line-height: 1.15;
}
.nowcard-meta { margin: 0 0 1rem; color: var(--text-muted); font-size: 0.85rem; }
.nowcard-actions { display: flex; align-items: center; flex-wrap: wrap; gap: 0.6rem; }
.actions-spacer { flex: 1; }

.btn-primary,
.btn-outline,
.btn-chip {
  border-radius: 999px;
  font: inherit;
  cursor: pointer;
  transition: background 120ms ease, border-color 120ms ease, color 120ms ease;
}
.btn-primary {
  background: var(--sun);
  color: var(--accent-fg);
  border: 1px solid transparent;
  font-weight: 700;
  padding: 0.62rem 1.6rem;
  box-shadow: inset 0 1px 0 rgb(255 255 255 / 0.25);
}
.btn-primary:hover:not(:disabled) { background: color-mix(in srgb, var(--sun) 88%, var(--text)); }
.btn-outline {
  background: none;
  border: 1px solid var(--border-input);
  color: var(--text-muted);
  padding: 0.58rem 1.05rem;
}
.btn-outline:hover:not(:disabled) { color: var(--text); border-color: var(--text-muted); }
.btn-chip {
  background: none;
  border: 1px solid var(--border);
  color: var(--text-muted);
  padding: 0.5rem 0.75rem;
  font-size: 0.85rem;
  font-variant-numeric: tabular-nums;
}
.btn-chip:hover:not(:disabled) { color: var(--text); border-color: var(--border-input); }

.ev-more-wrap { position: absolute; top: 0.55rem; right: 0.6rem; }
.ev-more {
  display: grid;
  place-items: center;
  width: 2rem;
  height: 2rem;
  background: none;
  border: none;
  border-radius: 50%;
  color: var(--text-muted);
  font: inherit;
  line-height: 1;
  cursor: pointer;
  transition: background 120ms ease, color 120ms ease;
}
.ev-more:hover { background: var(--sunk); color: var(--text); }
.ev-menu {
  position: absolute;
  top: calc(100% + 4px);
  right: 0;
  z-index: 5;
  min-width: 9rem;
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  padding: 0.25rem;
  box-shadow: 0 10px 28px color-mix(in srgb, var(--text) 18%, transparent);
}
.ev-menu-item {
  display: block;
  width: 100%;
  text-align: left;
  background: none;
  border: none;
  border-radius: calc(var(--radius) - 4px);
  padding: 0.5rem 0.7rem;
  color: var(--text);
  font: inherit;
  font-size: 0.9rem;
  cursor: pointer;
}
.ev-menu-item:hover:not(:disabled) { background: var(--sunk); }

.today-clear {
  margin: 0 0 0.9rem -0.9rem;
  color: var(--text-muted);
  font-family: var(--serif);
  font-size: 1.1rem;
}
.today-tomorrow { margin: 1.9rem 0 0; color: var(--text-muted); font-size: 0.85rem; }

/* a finger needs more than the pointer does */
@media (pointer: coarse) {
  .btn-primary, .btn-outline, .btn-chip, .ev-menu-item, .toast-action { min-height: 2.5rem; }
  .btn-chip { min-width: 2.5rem; }
  .ev-more { width: 2.5rem; height: 2.5rem; }
}

@media (max-width: 767.98px) {
  .spine { padding-left: 4rem; }
  .spine::before { left: 2.9rem; }
  .ev-time { left: -4rem; width: 2.4rem; }
  .ev-dot { left: -1.2rem; }
  .now-line { margin-left: -4rem; }
  .now-dot { left: 2.6rem; }
  .now-label { left: 3.9rem; }
  .nowcard { margin-left: -1.9rem; padding: 1.05rem 1.05rem 0.95rem; }
  .nowcard-title { font-size: 1.55rem; }
}
```

- [ ] **Step 2: Update the reduced-motion block**

Replace `web/src/styles.css:543-545` (`.event-card, .task-row, .check, button.ghost`) with:

```css
@media (prefers-reduced-motion: reduce) {
  .task-row, .check, button.ghost,
  .debrief-fold, .btn-primary, .btn-outline, .btn-chip, .ev-more { transition: none; }
}
```

- [ ] **Step 3: Build and type-check**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: `tsc` silent, `vite build` reports `✓ built`.

- [ ] **Step 4: Commit**

```bash
git add web/src/styles.css
git commit -m "feat: Daylight Today styles for the Now line, Now card, and folded letter"
```

---

### Task 4: Screenshot harness and visual verification

**Files:**
- Create (scratchpad only, not committed): a Node mock-API server and a chromium screenshot script.

**Interfaces:**
- Consumes: `web/dist` from Task 3's build.
- Produces: PNGs proving the four states — desktop light with a pending Now card, dark mobile, the ⋯ menu open, and the all-settled `That's everything today.` state.

- [ ] **Step 1: Write the mock server**

Serve `web/dist` statically and stub the three endpoints Today calls:

```js
// scratchpad/serve.mjs
import http from 'node:http'
import fs from 'node:fs'
import path from 'node:path'

const DIST = '/home/shuntia/Projects/note/web/dist'
const CASE = process.env.CASE ?? 'normal'
const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.svg': 'image/svg+xml', '.png': 'image/png' }

const full = [
  { id: 1, kind: 'morning_checkin', wall_time: '07:30', status: 'done', flexibility: 'slide', slide_window_min: 30, channel: 'push' },
  { id: 2, kind: 'meds', wall_time: '08:00', status: 'done', flexibility: 'fixed', slide_window_min: 0, channel: 'push' },
  { id: 3, kind: 'lunch_reset', wall_time: '12:30', status: 'done', flexibility: 'slide', slide_window_min: 30, channel: 'push' },
  { id: 4, kind: 'errand_run', wall_time: '13:45', status: 'dropped', flexibility: 'drop', slide_window_min: 0, channel: 'push' },
  { id: 5, kind: 'afternoon_checkin', wall_time: '15:30', status: 'pending', flexibility: 'slide', slide_window_min: 30, channel: 'push' },
  { id: 6, kind: 'wind_down_walk', wall_time: '18:00', status: 'pending', flexibility: 'slide', slide_window_min: 30, channel: 'push' },
  { id: 7, kind: 'debrief', wall_time: '21:30', status: 'pending', flexibility: 'fixed', slide_window_min: 0, channel: 'push' },
]
const cases = {
  normal: full,
  fired: full.map((e) => (e.id === 5 ? { ...e, status: 'fired' } : e)),
  clear: full.map((e) => (['pending', 'fired'].includes(e.status) ? { ...e, status: 'done' } : e)),
}

http
  .createServer((req, res) => {
    const url = req.url.split('?')[0]
    const json = (v) => { res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify(v)) }
    if (url === '/api/me') return json({ username: 'shuntia', admin: true })
    if (url === '/api/plan/today') return json(cases[CASE])
    if (url === '/api/debrief') return json({ date: '2026-09-01', content: 'Slept seven hours and two check-ins landed yesterday. Today is light — one deep-work block and a walk at six.\n\nThe errand run slid off; it can wait for Tuesday.' })
    if (url.startsWith('/api/')) { res.writeHead(404); return res.end('{}') }
    const file = url === '/' ? '/index.html' : url
    const abs = path.join(DIST, file)
    if (!fs.existsSync(abs) || fs.statSync(abs).isDirectory()) { res.writeHead(200, { 'content-type': 'text/html' }); return res.end(fs.readFileSync(path.join(DIST, 'index.html'))) }
    res.writeHead(200, { 'content-type': TYPES[path.extname(abs)] ?? 'application/octet-stream' })
    res.end(fs.readFileSync(abs))
  })
  .listen(4173)
```

- [ ] **Step 2: Shoot the desktop light case**

```bash
chromium --headless --disable-gpu --no-sandbox --hide-scrollbars \
  --window-size=1280,1000 --screenshot=/tmp/.../today-desktop-light.png \
  --virtual-time-budget=4000 http://127.0.0.1:4173/
```

Confirm by eye: folded letter row on top, dashed Now line with sun disc and `NOW · HH:MM`, one enlarged card under it, exactly one filled amber button (Done), a ⋯ in the card corner, no actions on any other row, `Tomorrow's plan arrives overnight — nothing for you to set up.` at the bottom.

- [ ] **Step 3: Shoot dark mobile, the open ⋯ menu, and the all-settled case**

Dark mobile: `--window-size=414,900` with a prelude that sets `localStorage['note.theme'] = 'dark'` (load once, set, reload). Open menu: drive it with a small CDP/`--dump-dom` variant or a second load that clicks `.ev-more` before capture. All-settled: rerun the server with `CASE=clear`.

- [ ] **Step 4: Read each PNG and record pass/fail against the two acceptance checklists.**

- [ ] **Step 5: No commit — the harness lives in the scratchpad.**

---

### Task 5: Full verification and final commit

- [ ] **Step 1: Type-check**

Run: `cd web && npx tsc --noEmit`
Expected: no output.

- [ ] **Step 2: Build**

Run: `cd web && npx vite build`
Expected: `✓ built in …`.

- [ ] **Step 3: Rust suite (unchanged, but the constraint requires proving it)**

Run: `cargo test --workspace`
Expected: every suite `ok`, 225 tests total.

- [ ] **Step 4: Grep for forbidden colour**

Run: `grep -n 'danger\|red' web/src/views/Today.tsx`
Expected: no matches.

- [ ] **Step 5: Commit any remaining changes**

```bash
git add -A web docs/superpowers/plans/2026-09-01-daylight-step-1-2-today.md
git commit -m "feat: Daylight steps 1-2 — Today action hierarchy, Now line and Now card"
```

---

## Self-Review

**Spec coverage**

| Spec item | Task |
|---|---|
| 1.1 only the current event shows actions | Task 2 — `currentIndex`, `EventRow` renders no buttons |
| 1.2 Done filled / Later outlined / ±15 chips at the far end / Drop behind ⋯ | Task 2 `NowCard` + `Overflow`, Task 3 `.btn-primary`/`.btn-outline`/`.btn-chip`/`.actions-spacer` |
| 1.2 drop toast + 5 s Undo | Task 1 toast action, Task 2 `heldDrop` / `drop` |
| 1.3 existing API semantics | Task 2 keeps `eventAction`/`snooze(30)`/`shift(±15)` |
| 2.1 Now line: dashed rule, 12 px sun disc, `NOW · HH:MM`, minute updates | Task 3 `.now-line`, Task 2 minute-aligned tick |
| 2.2 Now card: serif title, eyebrow, meta line, action row | Task 2 `NowCard`, Task 3 `.nowcard*` |
| 2.3 past done/dropped compact + faded | Task 2 `EventRow`, Task 3 `.ev.done` / `.ev.dropped` |
| 2.4 future compact with flexibility tag | Task 2 `flexTag` |
| 2.5 debrief folded row, per-day localStorage | Task 2 `DebriefFold` / `readFold` / `writeFold` |
| 2.6 tomorrow line | Task 2 |
| `That's everything today.` | Task 2 `spine` when `current === -1` |
| a11y floor | Task 3 `(pointer: coarse)` block, global `:focus-visible`, reduced-motion block, `--text-muted` for contrast |

**Placeholder scan:** no TBD/TODO; every code step carries the literal source.

**Type consistency:** `ToastAction` in Task 1 matches the object literal passed in Task 2's `drop`. `currentIndex` returns `-1` and both `spine` call sites handle it. `EventRow`'s `above` prop is passed at its only call site. `firstSentence` returns `{ lead, more }` and is destructured that way.

**Known gap, deliberately deferred:** spec 2.3 asks a dropped row to read `dropped — <where it went>` when the agent rescheduled it. `PlanEvent` carries no such field and adding one is a server change, which this step forbids. The row renders the `else` branch — plain `dropped` — until that data exists.
