# Daylight Step 7 (UI half) — Blocks on Today, bells in Settings

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render the two shapes the server now serves — time-ranged blocks and routines that may or may not ping — on Today, and let Settings' Schedule pane show one row per template entry with a bell toggle that writes back.

**Architecture:** The plan API's new fields (`entry`, `end_wall_time`, `alert`, `moved_to`) land in `web/src/types.ts`, and `Today.tsx` grows two rendering branches: a dashed band for a block and a bell glyph after a routine's name. A block is never the Now card — `currentIndex` skips it — which is what keeps ±15 and Later off a band without any new guard. Settings gains a `Schedule` list under the three existing fields, whose toggles write `{"alerts":[…]}` through `PUT /api/settings` on their own, so the index they name always refers to the template whose rows are on screen.

**Tech Stack:** React 19 + TypeScript (strict), Vite, hand-written CSS with the existing token set in `web/src/styles.css`.

**Spec:** `docs/superpowers/plans/2026-09-01-daylight-ui-spec.md` — step 7 items 7.3 and 7.4, plus step 2 item 3's `dropped — <where it went>`. Server contract: `docs/superpowers/plans/2026-09-01-daylight-step-7a-blocks-alerts.md` ("The contract the UI agent codes against"). Mockups: `docs/superpowers/mockups/daylight/today-desktop-light.html`, `today-mobile-dark.html`, `settings-desktop-light.html`.

**Out of scope:** all Rust (7.1, 7.2, 7.5 shipped); new tokens, fonts and the theme pass (step 10); the Now screen's `Next` line, which may still name a block.

## Global Constraints

- Existing design tokens only — no new fonts, no new global tokens (that is step 10). Accent = amber only; moss = done; clay = dropped/warn. **Never introduce red.**
- Where this plan and a mockup disagree on a value, the mockup wins for look; the spec wins for behavior.
- Copy, exactly: `flexible — Note may reshape it`, `Routines & blocks — choose which ones ping you`, `pings you` / `silent` behind a bell glyph, `blocks never ping`, `Silent routines still appear on Today — they just don't send a push.`, `dropped — moved to <wall_time>`.
- The bell is the mockups' vector glyph, not the 🔔/🔕 emoji: the system emoji font draws 🔕 with a red slash, and the palette admits no red.
- Step 9 strings already in the Schedule pane must not regress: `Your days start and end here. Type a city to search.`, `When Note writes the morning letter and plans tomorrow.`, `Shape of the day` / `Which routines and blocks make up a day.`
- Accessibility floor: visible keyboard focus on every interactive element; toggles operable by keyboard and labelled for screen readers (never an unlabelled emoji); hit targets ≥ 40×40 px on touch layouts; text contrast ≥ 4.5:1; `prefers-reduced-motion` disables decorative animation.
- Comment policy: comment only what the code cannot say for itself; no process-history narration.
- `cd web && npx tsc --noEmit && npx vite build` must pass at every commit. No Rust changes: `cargo test --workspace` stays at 264.

---

## File Structure

- `web/src/types.ts` — `PlanEvent` gains `end_wall_time`, `entry`, `alert`, `moved_to`; new `MovedTo` and `ScheduleRow`; `Settings` gains `schedule`.
- `web/src/api.ts` — `saveSettings` takes an optional alerts list and returns the saved body.
- `web/src/bell.tsx` — the bell glyph, struck when silent; Today and Settings both draw it.
- `web/src/views/Today.tsx` — band rendering, bells, the `moved_to` drop tag, and a `currentIndex` that skips blocks.
- `web/src/views/Settings.tsx` — the Schedule pane's routines-and-blocks list; Save becomes the sun pill and reports `✓ Saved`.
- `web/src/styles.css` — `.ev-band`, `.ev-bell`, `.sched-*`; the coarse-pointer target list gains the toggle.

---

### Task 1: The plan and settings shapes the client reads

**Files:**
- Modify: `web/src/types.ts`, `web/src/api.ts:22-24,115-124`

**Interfaces:**
- Produces:
  - `MovedTo = { event_id: number; date: string; wall_time: string; kind: string }`
  - `PlanEvent` gains `end_wall_time: string | null`, `entry: 'routine' | 'block'`, `alert: boolean`, `moved_to?: MovedTo`
  - `ScheduleRow = { index: number; kind: string; entry: 'routine' | 'block'; time: string; end_time: string | null; days: string[]; flexibility: 'fixed' | 'slide' | 'drop'; slide_window_min: number; channel: string; alert: boolean }`
  - `Settings` gains `schedule: ScheduleRow[]`
  - `AlertPatch = { index: number; alert: boolean }`
  - `SettingsSaved = Pick<Settings, 'display_name' | 'timezone' | 'nightly_time' | 'template'> & { schedule: ScheduleRow[] }`
  - `api.saveSettings(patch: SettingsPatch, alerts?: AlertPatch[]): Promise<SettingsSaved>`
- Consumes: the server contract in `2026-09-01-daylight-step-7a-blocks-alerts.md`.

There is no test runner in `web/`; `npx tsc --noEmit` is the check that the shapes hold, and the browser walk in Task 5 is the behavioral one.

- [ ] **Step 1: Add the types**

In `web/src/types.ts`, above `PlanEvent`:

```ts
export type MovedTo = { event_id: number; date: string; wall_time: string; kind: string }

export type PlanEvent = {
  id: number
  kind: string
  wall_time: string
  end_wall_time: string | null
  entry: 'routine' | 'block'
  status: 'pending' | 'fired' | 'snoozed' | 'done' | 'dropped'
  flexibility: 'fixed' | 'slide' | 'drop'
  slide_window_min: number
  channel: string
  alert: boolean
  // absent unless the agent named the event this one moved to
  moved_to?: MovedTo
}
```

and beside `Settings`:

```ts
export type ScheduleRow = {
  index: number
  kind: string
  entry: 'routine' | 'block'
  time: string
  end_time: string | null
  days: string[]
  flexibility: 'fixed' | 'slide' | 'drop'
  slide_window_min: number
  channel: string
  alert: boolean
}

export type Settings = {
  display_name: string
  timezone: string
  nightly_time: string
  template: string
  templates: string[]
  timezones: string[]
  schedule: ScheduleRow[]
}
```

- [ ] **Step 2: Let `saveSettings` carry the bells**

In `web/src/api.ts`, add to the imports `AlertPatch`-free names only (`ScheduleRow` is not needed here if `SettingsSaved` lives in `types.ts`; put `AlertPatch` and `SettingsSaved` in `types.ts` and import both). Then:

```ts
export type AlertPatch = { index: number; alert: boolean }

export type SettingsSaved = Pick<
  Settings,
  'display_name' | 'timezone' | 'nightly_time' | 'template'
> & { schedule: ScheduleRow[] }
```

in `types.ts`, and in `api.ts`:

```ts
  // The server rejects unknown fields, so only the writable keys actually set go on the wire.
  // Bell toggles apply to the template the request leaves selected.
  saveSettings: (patch: SettingsPatch, alerts?: AlertPatch[]) => {
    const body: SettingsPatch & { alerts?: AlertPatch[] } = {}
    for (const key of WRITABLE_SETTINGS) {
      const value = patch[key]
      if (value !== undefined) body[key] = value
    }
    if (alerts?.length) body.alerts = alerts
    return request<SettingsSaved>('/api/settings', { method: 'PUT', body: JSON.stringify(body) })
  },
```

- [ ] **Step 3: Typecheck**

Run: `cd web && npx tsc --noEmit`
Expected: PASS. (`Settings.tsx` ignores the new return value; `Today.tsx` reads none of the new fields yet.)

- [ ] **Step 4: Commit**

```bash
git add web/src/types.ts web/src/api.ts
git commit -m "feat: the client reads blocks, bells, and drop destinations"
```

---

### Task 2: Today renders blocks, bells, and where a drop went

**Files:**
- Modify: `web/src/views/Today.tsx`, `web/src/styles.css`

**Interfaces:**
- Consumes: Task 1's `PlanEvent`.
- Produces: nothing importable; the rendering contract is the class names Task 5 asserts on — `.ev.block`, `.ev-band`, `.ev-range`, `.ev-bell`, `.ev-tag`.

Behavior, from spec 7.3 and the mockups:

1. A block renders as a dashed-border band spanning its range: name, `09:30 – 12:30` range, and the tag `flexible — Note may reshape it` pushed to the far end. Mobile shows the short tag `flexible` instead. Both tags are always in the DOM; a media query decides which is visible, so nothing depends on a JS viewport read.
2. A block whose `end_wall_time` has passed gets `.past` and fades (mockup: `color: faint`, border back to plain `--mist`).
3. A routine carries a bell glyph after its name — the mockup's inline SVG, struck (`M3 3l18 18`) when `alert` is false. `@media (max-width: 767.98px)` hides it, per the mobile mockup.
4. A dropped event's tag reads `dropped — moved to <wall_time>` when `moved_to` is present, `dropped` otherwise.
5. `currentIndex` skips blocks, so a block is never the Now card and therefore never renders Done/Later/±15/⋯. This is the whole of "a block offers no ±15 chips and no Later" — `POST /shift` and `/snooze` 404 on a block, and nothing in the UI can reach them.
6. A silent routine that is the Now card drops the `reaches you as a push` clause from its meta line rather than claiming a push it will not send.

- [ ] **Step 1: Skip blocks when choosing the Now card**

Replace `currentIndex` in `web/src/views/Today.tsx`:

```ts
// The fired event owns Now; failing that, the next routine still open does. A
// block spans time rather than arriving at it, so it never takes the card.
function currentIndex(events: PlanEvent[]): number {
  const fired = events.findIndex((ev) => ev.status === 'fired')
  if (fired !== -1) return fired
  return events.findIndex(
    (ev) => ev.entry !== 'block' && (ev.status === 'pending' || ev.status === 'snoozed'),
  )
}
```

- [ ] **Step 2: Tell the truth in the Now card's meta line**

```ts
function metaLine(ev: PlanEvent): string {
  return [slideText(ev), ev.alert ? reachText(ev.channel) : ''].filter(Boolean).join(' · ')
}
```

- [ ] **Step 3: Add the bell and the dropped tag**

Create `web/src/bell.tsx` — Settings draws the same glyph in Task 3:

```tsx
const BODY = 'M18 8a6 6 0 0 0-12 0c0 7-3 9-3 9h18s-3-2-3-9'
const CLAPPER = 'M13.7 21a2 2 0 0 1-3.4 0'

// Struck when the routine is silent. Without a `label` the glyph is decoration —
// the control around it is carrying the name.
export function Bell({ on, label }: { on: boolean; label?: string }) {
  return (
    <svg
      className="bell"
      viewBox="0 0 24 24"
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      <path d={BODY} />
      <path d={CLAPPER} />
      {!on && <path d="M3 3l18 18" />}
    </svg>
  )
}
```

and above `EventRow` in `Today.tsx` (which imports `Bell` from `../bell`):

```tsx
function droppedTag(ev: PlanEvent): string {
  return ev.moved_to ? `dropped — moved to ${ev.moved_to.wall_time}` : 'dropped'
}
```

- [ ] **Step 4: Split `EventRow` into a band and a row**

```tsx
function EventRow({ ev, now }: { ev: PlanEvent; now: string }) {
  if (ev.entry === 'block') return <BlockBand ev={ev} now={now} />
  const state = ev.status === 'done' ? 'done' : ev.status === 'dropped' ? 'dropped' : ''
  const tag = state === 'dropped' ? droppedTag(ev) : state === 'done' ? '' : flexTag(ev)
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
        <Bell on={ev.alert} label={ev.alert ? 'pings you' : 'silent'} />
        <span className="ev-tag">{tag}</span>
      </div>
    </li>
  )
}

function BlockBand({ ev, now }: { ev: PlanEvent; now: string }) {
  const end = ev.end_wall_time ?? ev.wall_time
  const past = minutesOf(end) <= minutesOf(now)
  return (
    <li className={`ev block${past ? ' past' : ''}`}>
      <span className="ev-time">{ev.wall_time}</span>
      <span className="ev-dot" aria-hidden="true" />
      <div className="ev-band">
        <span className="ev-name">{eventLabel(ev.kind)}</span>
        <span className="ev-range">
          {ev.wall_time} – {end}
        </span>
        <span className="ev-tag long">flexible — Note may reshape it</span>
        <span className="ev-tag short">flexible</span>
      </div>
    </li>
  )
}
```

`spine()` already has `now` in scope; pass it: `rows.push(<EventRow key={ev.id} ev={ev} now={now} />)`.

- [ ] **Step 5: Style the band and the bell**

In `web/src/styles.css`, after the `.ev.dropped .ev-tag` rule:

```css
/* a block owns a span of the day rather than a moment in it: a dashed band the
   agent may reshape, faded once its end is behind us */
.ev-band {
  display: flex;
  align-items: center;
  gap: 0.55rem;
  padding: 0.55rem 0.85rem;
  background: color-mix(in srgb, var(--sunk) 62%, var(--surface));
  border: 1px dashed color-mix(in srgb, var(--sun) 24%, var(--border));
  border-radius: 12px;
  color: var(--text-muted);
  font-size: 0.95rem;
}
.ev.block .ev-dot { background: transparent; border-color: var(--border-input); }
.ev.block.past .ev-band { border-color: var(--border); opacity: 0.62; }
.ev-range { font-size: 0.78rem; font-variant-numeric: tabular-nums; }
.ev-band .ev-tag { margin-left: auto; white-space: nowrap; }
.ev-tag.short { display: none; }

/* whether this routine may reach for your attention, drawn the same way wherever
   it is asked: struck when it stays quiet */
.bell {
  width: 13px;
  height: 13px;
  flex: none;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.9;
  stroke-linecap: round;
  stroke-linejoin: round;
}
```

and inside the existing `@media (max-width: 767.98px)` block for Today (scoped to the
spine, so the Settings pill keeps its glyph at every width):

```css
  /* the phone layout drops the bells and shortens the block's tag */
  .ev-row .bell { display: none; }
  .ev-tag.long { display: none; }
  .ev-tag.short { display: inline; }
```

- [ ] **Step 6: Typecheck and build**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: both PASS.

- [ ] **Step 7: Commit**

```bash
git add web/src/views/Today.tsx web/src/styles.css
git commit -m "feat: today shows blocks as bands, bells on routines, and where a drop went"
```

---

### Task 3: The Schedule pane lists routines and blocks

**Files:**
- Modify: `web/src/views/Settings.tsx:94-277`, `web/src/styles.css`

**Interfaces:**
- Consumes: Task 1's `ScheduleRow`, `AlertPatch`, `api.saveSettings`.
- Produces: the class names Task 5 asserts on — `.sched`, `.sched-head`, `.sched-row`, `.sched-toggle`, `.sched-na`, `.sched-note`.

Behavior, from spec 7.4 and the settings mockup:

1. Under the three existing fields, the heading `Routines & blocks — choose which ones ping you`, then one row per `schedule` entry in template order: a time/flexibility gutter, the row's `kind` as its name, and a trailing control.
2. A routine's control is a toggle pill: a bell and `pings you` (sun-tinted) or a struck bell and `silent` (muted), `aria-pressed` carrying the state and an `aria-label` naming the row, so the pill is never a bare glyph to a screen reader.
3. A block's row shows the tag `flexible — Note may reshape it` and, in place of a toggle, `blocks never ping`.
4. Footnote: `Silent routines still appear on Today — they just don't send a push.`
5. Toggling writes immediately: the pill flips first, then `PUT /api/settings` carries `{"alerts":[{"index":i,"alert":b}]}` and nothing else, so the index always refers to the template that is saved — the one whose rows are on screen. Success adopts the response's `schedule`; failure puts the old rows back and shows the server's message.
6. An empty `schedule` (a template that no longer parses) renders no heading and no list — the picker above it is what the user needs.
7. Save becomes the filled sun pill (`btn-primary`) and success reads `✓ Saved` inline in moss, never a dialog.

- [ ] **Step 1: Carry the rows in the pane's state**

In `web/src/views/Settings.tsx`, extend the loaded state and the import list (`ScheduleRow` from `../types`, `AlertPatch` from `../types`):

```ts
type Loaded = { choices: Choices; baseline: Draft; draft: Draft; rows: ScheduleRow[] }
```

and in `load`:

```ts
        setState({
          choices: { templates: s.templates, timezones: s.timezones },
          baseline: draftOf(s),
          draft: draftOf(s),
          rows: s.schedule,
        }),
```

The two `setState` updaters inside `submit` spread `s`, so `rows` travels through the save untouched; after a successful save adopt the server's list by adding `rows: saved.schedule` to the returned object, where `saved` is the awaited `api.saveSettings(patch)`.

- [ ] **Step 2: Write the toggle handler**

Inside `ProfileAndSchedule`, beside `submit`:

```ts
  // A toggle carries no other field, so the index it names always addresses the
  // template that is saved — the one whose rows are on screen.
  const toggleAlert = async (row: ScheduleRow) => {
    const next = !row.alert
    setSave({ kind: 'busy' })
    setState((s) =>
      s && s !== 'error'
        ? { ...s, rows: s.rows.map((r) => (r.index === row.index ? { ...r, alert: next } : r)) }
        : s,
    )
    try {
      const saved = await api.saveSettings({}, [{ index: row.index, alert: next }])
      setState((s) => (s && s !== 'error' ? { ...s, rows: saved.schedule } : s))
      setSave({ kind: 'saved' })
    } catch (err) {
      setState((s) =>
        s && s !== 'error'
          ? { ...s, rows: s.rows.map((r) => (r.index === row.index ? { ...r, alert: row.alert } : r)) }
          : s,
      )
      setSave({
        kind: 'failed',
        message:
          err instanceof ApiError && err.status === 400
            ? err.message
            : "That didn't save. Try again.",
      })
    }
  }
```

- [ ] **Step 3: Render the list**

Add above `ProfileAndSchedule`:

```tsx
function flexWord(row: ScheduleRow): string {
  if (row.flexibility === 'fixed') return 'fixed'
  if (row.flexibility === 'drop') return 'droppable'
  return row.slide_window_min > 0 ? `±${row.slide_window_min}m` : 'flexible'
}

function ScheduleRows({
  rows,
  busy,
  toggle,
}: {
  rows: ScheduleRow[]
  busy: boolean
  toggle: (row: ScheduleRow) => void
}) {
  if (rows.length === 0) return null
  return (
    <div className="sched">
      <h3 className="sched-head">Routines &amp; blocks — choose which ones ping you</h3>
      <ul className="sched-list">
        {rows.map((row) => (
          <li className="sched-row" key={row.index}>
            <span className="sched-time mono">
              {row.entry === 'block' ? `${row.time}–${row.end_time ?? ''}` : `${row.time} · ${flexWord(row)}`}
            </span>
            <span className="sched-name">{row.kind}</span>
            {row.entry === 'block' ? (
              <>
                <span className="sched-flex">flexible — Note may reshape it</span>
                <span className="sched-na">blocks never ping</span>
              </>
            ) : (
              <button
                type="button"
                className={`sched-toggle ${row.alert ? 'on' : 'off'}`}
                aria-pressed={row.alert}
                aria-label={`${row.kind} — ${row.alert ? 'pings you' : 'silent'}`}
                disabled={busy}
                onClick={() => toggle(row)}
              >
                <Bell on={row.alert} />
                {row.alert ? 'pings you' : 'silent'}
              </button>
            )}
          </li>
        ))}
      </ul>
      <p className="sched-note muted">
        Silent routines still appear on Today — they just don't send a push.
      </p>
    </div>
  )
}
```

and mount it directly after the `Shape of the day` row's closing `</div>`, still inside the schedule branch but **outside** the `<fieldset>` — a `disabled` fieldset would swallow the toggles' own busy state:

```tsx
          )}
        </fieldset>
        {!profile && <ScheduleRows rows={state.rows} busy={save.kind === 'busy'} toggle={toggleAlert} />}
```

(`state` is narrowed to `Loaded` by this point in the function; use the destructured `rows` if you prefer — add `rows` to the existing `const { choices, baseline, draft } = state` destructure.)

- [ ] **Step 4: Fix the Save button and its status**

In the same `pane-foot`, and in `PersonaSection`'s `pane-foot`, the primary button becomes the pill and the status reads `✓ Saved`:

```tsx
          <button className="btn-primary" disabled={!dirty || save.kind === 'busy'}>
            {save.kind === 'busy' ? 'Saving…' : 'Save changes'}
          </button>
          {save.kind === 'saved' && (
            <span className="pane-status ok" role="status">
              ✓ Saved
            </span>
          )}
```

- [ ] **Step 5: Style the list**

In `web/src/styles.css`, after the `.set-prompt-note` rule:

```css
/* the day's shape, entry by entry: when each one lands, what it is, and whether
   it may reach for your attention */
.sched { margin-top: 1.1rem; }
.sched-head { margin: 0 0 0.5rem; font-size: 0.9rem; font-weight: 700; }
.sched-list { list-style: none; margin: 0; padding: 0; }
.sched-row {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 0.6rem;
  padding: 0.5rem 0.2rem;
  border-top: 1px solid var(--border);
  font-size: 0.92rem;
}
.sched-row:last-child { border-bottom: 1px solid var(--border); }
.sched-time { flex: none; width: 7.5rem; font-size: 0.78rem; color: var(--text-muted); }
.sched-name { min-width: 0; }
.sched-flex,
.sched-na { font-size: 0.72rem; color: var(--text-muted); }
.sched-flex { border: 1px solid var(--border); border-radius: 999px; padding: 0.05rem 0.5rem; }
.sched-na { margin-left: auto; }
.sched-toggle {
  margin-left: auto;
  display: inline-flex;
  align-items: center;
  gap: 0.4rem;
  background: none;
  border: 1px solid var(--border-input);
  border-radius: 999px;
  padding: 0.25rem 0.75rem;
  color: var(--text-muted);
  font: inherit;
  font-size: 0.78rem;
  cursor: pointer;
  transition: border-color 120ms ease, color 120ms ease, background 120ms ease;
}
.sched-toggle.on {
  color: var(--sun-ink);
  border-color: color-mix(in srgb, var(--sun) 35%, var(--border));
  background: color-mix(in srgb, var(--sun) 10%, transparent);
}
.sched-toggle:hover:not(:disabled) { border-color: var(--sun); }
.sched-note { margin: 0.55rem 0 0; font-size: 0.78rem; }

@media (prefers-reduced-motion: reduce) {
  .sched-toggle { transition: none; }
}
```

and add `.sched-toggle` to the coarse-pointer minimum-target rule:

```css
  .btn-primary, .btn-outline, .btn-chip, .ev-menu-item, .toast-action, .capture,
  .sched-toggle { min-height: 2.5rem; }
```

- [ ] **Step 6: Typecheck and build**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: both PASS.

- [ ] **Step 7: Commit**

```bash
git add web/src/views/Settings.tsx web/src/styles.css
git commit -m "feat: settings schedule pane toggles which routines ping"
```

---

### Task 4: Prove it against a stub API in a real browser

**Files:**
- Create: `/tmp/claude-*/scratchpad/stub/` (server + fixtures; scratchpad only, never committed)

**Interfaces:**
- Consumes: the built bundle in `web/dist`.
- Produces: screenshots and DOM assertions for the report.

Playwright's bundled Chromium cannot start on this host (`libgbm.so.1` missing); drive the system `chromium` binary through `playwright-core`'s `executablePath`, as earlier sessions did.

- [ ] **Step 1: Write the stub server**

A single Node script serving `web/dist` plus canned JSON:

- `GET /api/me` → `{"username":"aki","admin":false}`
- `GET /api/debrief` → 404
- `GET /api/plan/today` → a done routine with `alert:true`; a done routine with `alert:false`; a past block `09:30–12:30`; a dropped routine **with** `moved_to`; a dropped routine **without**; a live block spanning now; a pending routine (the Now card); a future routine.
- `GET /api/settings` → the four scalars, `templates`, `timezones`, and a `schedule` of five routines and one block.
- `PUT /api/settings` → echo the scalars plus the `schedule` with the requested alert applied; log the request body so the payload can be shown.
- `GET /api/tasks` → `[]`.

- [ ] **Step 2: Walk it**

Desktop (1280×900) and mobile (390×844) screenshots of Today; a Settings screenshot with Schedule selected; a click on a bell toggle followed by reading the logged PUT body and the `✓ Saved` status.

Assert in the page, not by eye alone:
- `.ev.block .ev-band` exists, its computed `border-style` is `dashed`, and it contains the range text and `flexible — Note may reshape it`.
- The past block's band has lower opacity than the live one.
- The block's row contains no `.btn-chip` and no button reading `Later`.
- `.ev-bell` count equals the routine count on desktop and `0` (`display: none`) at 390 px, where `.ev-tag.short` is the visible one.
- The tag text for the two dropped events is `dropped — moved to 17:00` and `dropped`.
- Settings: one `.sched-row` per schedule entry; the block row's text contains `blocks never ping` and no toggle; `.sched-note` carries the footnote; the Save button's computed `border-radius` is the 999px pill and its background is the sun.

- [ ] **Step 3: Record the results**

Every acceptance line in the report gets pass/fail plus the evidence that decided it.

---

### Task 5: Ship it

- [ ] **Step 1: Full verification**

Run: `cd web && npx tsc --noEmit && npx vite build`, then `cargo test --workspace` (expect 264, unchanged).

- [ ] **Step 2: Commit the plan alongside the work**

```bash
git add docs/superpowers/plans/2026-09-01-daylight-step-7-blocks-ui.md
git commit -m "docs: plan for the step 7 UI half"
```

---

## Self-review

- Spec 7.3 → Task 2 (bands, ranges, the tag, fading, bells, mobile). Spec 7.4 → Task 3 (the pane, the toggle, the footnote, the sun-pill Save, `✓ Saved`). Spec 2.3's `dropped — <where it went>` → Task 2 Step 3. 7.1/7.2/7.5 are the shipped server half.
- "A block offers no ±15 and no Later" is enforced structurally: only the Now card carries actions and `currentIndex` cannot select a block. No dead branch guards a route that would 404 anyway.
- Names used across tasks — `MovedTo`, `ScheduleRow`, `AlertPatch`, `SettingsSaved`, `saveSettings(patch, alerts?)`, `flexWord`, `ScheduleRows`, `BlockBand`, `Bell`, `droppedTag` — are each defined once, in the task that introduces them.
- No new tokens: the band, the bell and the pill are built from `--sun`, `--sun-ink`, `--sunk`, `--border`, `--border-input`, `--text-muted`, `--surface`. Nothing renders red.
