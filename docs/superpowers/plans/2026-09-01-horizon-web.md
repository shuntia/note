# Horizon Web Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild the web client as Horizon: one home screen with two faces and Today underneath, plus Tasks, Chat, Memory and Settings cut to what the user needs right now.

**Architecture:** The existing React shell keeps its state-only tab routing, toast/undo and WebSocket wiring. Today and Now are replaced by one `Home` view that owns the session lifecycle (from `Now.tsx`) and the day's events (from `Today.tsx`) and reveals itself in stages on swipe/scroll. A shared `Gauge` draws the arc, a shared `DayLine` draws the horizon, a shared `TellNote` posts to chat. The other views keep their logic and get new markup and CSS. The stylesheet keeps its alias layer (`--bg`, `--surface`, `--text`…) so untouched rules keep working while the base tokens become Horizon's.

**Tech Stack:** React 19, TypeScript strict (`noUnusedLocals`), Vite 7, one stylesheet `web/src/styles.css`, pnpm. No frontend test runner: the gate for every task is `pnpm build` (tsc + vite) plus a headless-Chromium screenshot through the harness in Task 0.

**Spec:** `docs/superpowers/plans/2026-09-01-horizon-ui-spec.md`. Boards: `docs/superpowers/mockups/horizon/*.dc.html` (open `index.html`). The server plan `docs/superpowers/plans/2026-09-01-horizon-server.md` must be complete before Task 2.

## Global Constraints

- The display rule: does the user need to know this right now, and can they not infer it otherwise? If not, it is not rendered. No explanatory copy anywhere. Sentence case.
- One fixed palette, no time-of-day colour shift, never red. Tokens (light): sky top `oklch(84% 0.035 230)`, sky mid `oklch(91% 0.025 200)`, earth `oklch(93.5% 0.016 78)`, haze `oklch(99% 0.004 80 / 0.86)`, ink `oklch(23% 0.03 255)`, quiet `oklch(40% 0.025 250)`, faint `oklch(52% 0.02 245)`, line `oklch(30% 0.03 250 / 0.45)`, track `oklch(30% 0.03 250 / 0.16)`, sun `oklch(80% 0.15 76)`, sun-ink `oklch(50% 0.13 55)`, sage `oklch(50% 0.09 150)`, rose `oklch(55% 0.07 30)`.
- Type: Bricolage Grotesque (display) over Atkinson Hyperlegible (body), both self-hosted from `web/src/fonts/`; zero external requests (check the Network tab of a screenshot run: only same-origin).
- One filled control per screen (ink pill). Mobile: no wordmark, no screen titles, no capture bar; five tabs. Desktop: wordmark `Note` with no dot, nav pills, capture bar.
- Undo instead of confirm; `prefers-reduced-motion` disables the breathe animation and the digit drift.
- Commits: imperative summary, `pnpm build` clean (run from `web/`) before each. Trailer: `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01UvaK6yJJPvZeJz6UYwJ9Rp`.
- Work from the worktree root `/home/shuntia/Projects/note/.claude/worktrees/note-horizon`; web commands from `web/`.

## File structure

- `web/scripts/shot.mjs` (new) — screenshot harness: starts the server on a temp config, logs in, screenshots a view at a size.
- `web/src/fonts/bricolage-grotesque.woff2` + `BricolageGrotesque-OFL.txt` (new); `fraunces-subset.woff2` + `Fraunces-OFL.txt` (deleted).
- `web/src/styles.css` — tokens rewritten; the Today, Now and shell sections replaced; the rest restyled in place.
- `web/src/gauge.tsx` (new) — the arc.
- `web/src/dayline.tsx` (new) — the horizon.
- `web/src/tellnote.tsx` (new) — the "Tell Note" line.
- `web/src/stage.ts` (new) — swipe/scroll/key stage hook.
- `web/src/prefs.ts` (new) — cached `counter` / `show_arc_between_sessions`.
- `web/src/views/Home.tsx` (new) — replaces `views/Now.tsx`; hosts the session and the wait faces and, on mobile, Today underneath.
- `web/src/views/Today.tsx` — rewritten as the desktop page (hero + day line + letter).
- `web/src/views/Now.tsx` — deleted in Task 5.
- `web/src/app.tsx` — shell: desktop top nav, mobile Home hosting, chrome hiding.
- `web/src/views/Tasks.tsx`, `Talk.tsx`, `Memory.tsx`, `Settings.tsx` — markup/CSS updates only.
- `web/index.html`, `web/public/manifest.webmanifest`, `web/src/theme.ts` — colours.

---

### Task 0: Screenshot harness

**Files:**
- Create: `web/scripts/shot.mjs`
- Modify: `web/package.json` (devDependency `playwright-core`, script `shot`), `web/vite.config.ts` (proxy target from `NOTE_API`)

**Interfaces:**
- Produces: `pnpm shot <tab> <width>x<height> <out.png> [--session]` — builds nothing; needs `pnpm dev` running on 5173 and the server it starts itself. `--session` seeds a focus session in localStorage before the shot.

- [ ] **Step 1: Add the dependency and script.** In `web/package.json` add `"playwright-core": "1.49.1"` under `devDependencies` and `"shot": "node scripts/shot.mjs"` under `scripts`. Run `cd web && pnpm install`. In `web/vite.config.ts` change the proxy target to `process.env.NOTE_API ?? 'http://127.0.0.1:3271'`.

- [ ] **Step 2: Write `web/scripts/shot.mjs`:**

```js
// Usage: node scripts/shot.mjs <today|tasks|chat|memory|settings> <WxH> <out.png> [--session]
// Expects `pnpm dev` on http://localhost:5173 with NOTE_API pointing at the server this
// script starts (default http://127.0.0.1:3299).
import { chromium } from 'playwright-core'
import { spawn, execFileSync } from 'node:child_process'
import { mkdtempSync, writeFileSync, cpSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

const [tab = 'today', size = '390x844', out = 'shot.png', ...flags] = process.argv.slice(2)
const [width, height] = size.split('x').map(Number)
const root = resolve(import.meta.dirname, '../..')
const port = Number(process.env.NOTE_PORT ?? 3299)

const dir = mkdtempSync(join(tmpdir(), 'note-shot-'))
cpSync(join(root, 'config/defaults'), join(dir, 'config/defaults'), { recursive: true })
writeFileSync(
  join(dir, 'config/server.toml'),
  `bind_addr = "127.0.0.1:${port}"\npublic_base_url = "http://127.0.0.1:${port}"\ndata_dir = "data"\n`,
)
const bin = join(root, 'target/debug/note-server')
execFileSync(bin, ['create-user', 'shot', 'shot-pass'], { cwd: dir, stdio: 'ignore' })
const server = spawn(bin, [], { cwd: dir, stdio: 'ignore' })
await new Promise((r) => setTimeout(r, 800))

const browser = await chromium.launch({ executablePath: process.env.CHROMIUM ?? '/etc/profiles/per-user/shuntia/bin/chromium' })
const page = await browser.newPage({ viewport: { width, height }, deviceScaleFactor: 1 })
const external = []
page.on('request', (req) => {
  const url = new URL(req.url())
  if (url.hostname !== 'localhost' && url.hostname !== '127.0.0.1') external.push(req.url())
})
await page.goto('http://localhost:5173/')
await page.fill('input[placeholder="Username"]', 'shot')
await page.fill('input[placeholder="Password"]', 'shot-pass')
await page.click('button:has-text("Sign in")')
await page.waitForSelector('nav[aria-label="Views"]', { timeout: 10000 })
if (flags.includes('--session')) {
  await page.evaluate(() => {
    localStorage.setItem(
      'note.nowSession',
      JSON.stringify({
        taskId: null, eventId: null, title: 'Email landlord about the leak', notes: '',
        stepIndex: 2, stepCount: 3, stepName: 'photos of the ceiling',
        durationSec: 1500, startedAt: Date.now() - 492_000, pausedAt: null, pausedMs: 0,
      }),
    )
  })
  await page.reload()
  await page.waitForTimeout(500)
}
if (tab !== 'today') {
  await page.click(`nav[aria-label="Views"] button:has-text("${tab[0].toUpperCase()}${tab.slice(1)}")`)
  await page.waitForTimeout(500)
}
await page.waitForTimeout(800)
await page.screenshot({ path: out })
console.log(`wrote ${out}`)
if (external.length) {
  console.error(`external requests:\n${external.join('\n')}`)
  process.exitCode = 1
}
await browser.close()
server.kill()
```

  Before the first use: `cargo build` (debug binary) from the repo root, then in one terminal `cd web && NOTE_API=http://127.0.0.1:3299 pnpm dev`, and in another `pnpm shot today 390x844 /tmp/today.png`. If `create-user` fails because the server reads `config/` relative to cwd differently, read `server/src/main.rs` for the config path flag and adjust the `cwd`/args above.

- [ ] **Step 3: Verify** the harness against the current (Daylight) UI: `pnpm shot today 1440x900 /tmp/base.png` produces a PNG of the sidebar shell; view it with the Read tool.

- [ ] **Step 4: Commit**

```bash
git add web/package.json web/pnpm-lock.yaml web/scripts/shot.mjs web/vite.config.ts
git commit -m "chore: headless screenshot harness for the web client"
```

---

### Task 1: Fonts and tokens

**Files:**
- Create: `web/src/fonts/bricolage-grotesque.woff2`, `web/src/fonts/BricolageGrotesque-OFL.txt`
- Delete: `web/src/fonts/fraunces-subset.woff2`, `web/src/fonts/Fraunces-OFL.txt`
- Modify: `web/src/styles.css` :1-140 (font faces + tokens), `web/src/fonts/README.md`, `web/index.html` (theme-color), `web/public/manifest.webmanifest`, `web/src/theme.ts`

**Interfaces:**
- Produces: CSS custom properties on `:root`: `--sky-top --sky-mid --earth --haze --haze-strong --ink --ivory --quiet --faint --line --track --sun --sun-ink --sage --rose --display --sans --mono --radius --radius-lg`, plus the alias layer `--bg --surface --surface-2 --text --text-muted --border --border-input --accent --ring --sidebar-bg --sidebar-border --bubble --code-bg` mapped onto them. `--dawn`, `--card`, `--sunk`, `--mist`, `--mist-strong`, `--moss`, `--clay`, `--spine` are kept as aliases (`--dawn: var(--earth)`, `--card: var(--haze-strong)`, `--sunk: var(--earth)`, `--mist: var(--line)`, `--mist-strong: var(--line)`, `--moss: var(--sage)`, `--clay: var(--rose)`) so every untouched rule still resolves.

- [ ] **Step 1: Fetch the font.** Google Fonts serves a variable woff2 when asked with a modern UA:

```bash
cd web/src/fonts
curl -sA "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/124 Safari/537.36" \
  "https://fonts.googleapis.com/css2?family=Bricolage+Grotesque:opsz,wght@12..96,300..700&display=swap" \
  | grep -o 'https://fonts.gstatic.com[^)]*latin[^)]*woff2' | head -1 | xargs -I{} curl -s -o bricolage-grotesque.woff2 {}
ls -la bricolage-grotesque.woff2   # expect roughly 40–90 KB
curl -s -o BricolageGrotesque-OFL.txt https://raw.githubusercontent.com/ateliertriay/bricolage/main/OFL.txt
rm fraunces-subset.woff2 Fraunces-OFL.txt
```

  If the grep finds several `latin` URLs (one per unicode-range), take the one whose preceding `unicode-range` line is the plain Latin block (`U+0000-00FF`). Update `web/src/fonts/README.md` to list Bricolage Grotesque (OFL, variable opsz/wght) and Atkinson Hyperlegible.

- [ ] **Step 2: Replace the `@font-face` block for Fraunces** (styles.css :23-29) with:

```css
@font-face {
  font-family: 'Bricolage Grotesque';
  src: url('./fonts/bricolage-grotesque.woff2') format('woff2');
  font-weight: 300 700;
  font-style: normal;
  font-display: swap;
}
```

- [ ] **Step 3: Replace `:root` (:31-88), `:root[data-theme='dark']` (:89-112) and the media block (:113-138)** with:

```css
:root {
  --sky-top: oklch(84% 0.035 230);
  --sky-mid: oklch(91% 0.025 200);
  --earth: oklch(93.5% 0.016 78);
  --haze: oklch(99% 0.004 80 / 0.55);
  --haze-strong: oklch(99% 0.004 80 / 0.86);
  --ink: oklch(23% 0.03 255);
  --ivory: oklch(98% 0.008 85);
  --quiet: oklch(40% 0.025 250);
  --faint: oklch(52% 0.02 245);
  --line: oklch(30% 0.03 250 / 0.45);
  --track: oklch(30% 0.03 250 / 0.16);
  --sun: oklch(80% 0.15 76);
  --sun-ink: oklch(50% 0.13 55);
  --sage: oklch(50% 0.09 150);
  --rose: oklch(55% 0.07 30);
  --sky: linear-gradient(180deg, var(--sky-top) 0%, var(--sky-mid) 45%, var(--earth) 72%);

  --radius: 12px;
  --radius-lg: 16px;
  --radius-xl: 18px;

  --display: 'Bricolage Grotesque', 'Avenir Next', 'Helvetica Neue', Arial, sans-serif;
  --sans: 'Atkinson Hyperlegible', system-ui, -apple-system, 'Segoe UI', sans-serif;
  --mono: ui-monospace, 'SF Mono', Menlo, monospace;

  /* aliases the older rules resolve through */
  --dawn: var(--earth);
  --card: var(--haze-strong);
  --sunk: var(--earth);
  --mist: var(--line);
  --mist-strong: var(--line);
  --moss: var(--sage);
  --clay: var(--rose);
  --accent-fg: var(--ink);
  --spine: none;
  --bg: var(--earth);
  --surface: var(--haze-strong);
  --surface-2: var(--earth);
  --text: var(--ink);
  --text-muted: var(--quiet);
  --border: var(--line);
  --border-input: var(--line);
  --accent: var(--sun);
  --ring: var(--sun-ink);
  --sidebar-bg: transparent;
  --sidebar-border: transparent;
  --bubble: var(--haze-strong);
  --code-bg: var(--earth);
}
:root[data-theme='dark'] {
  --sky-top: oklch(24% 0.03 250);
  --sky-mid: oklch(22% 0.025 240);
  --earth: oklch(20% 0.018 60);
  --haze: oklch(30% 0.02 60 / 0.6);
  --haze-strong: oklch(28% 0.02 60 / 0.92);
  --ink: oklch(93% 0.012 85);
  --ivory: oklch(20% 0.02 60);
  --quiet: oklch(74% 0.015 80);
  --faint: oklch(62% 0.014 75);
  --line: oklch(90% 0.01 80 / 0.35);
  --track: oklch(90% 0.01 80 / 0.14);
  --sun-ink: oklch(78% 0.13 78);
  --sage: oklch(72% 0.08 150);
  --rose: oklch(70% 0.06 30);
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme='light']) {
    --sky-top: oklch(24% 0.03 250);
    --sky-mid: oklch(22% 0.025 240);
    --earth: oklch(20% 0.018 60);
    --haze: oklch(30% 0.02 60 / 0.6);
    --haze-strong: oklch(28% 0.02 60 / 0.92);
    --ink: oklch(93% 0.012 85);
    --ivory: oklch(20% 0.02 60);
    --quiet: oklch(74% 0.015 80);
    --faint: oklch(62% 0.014 75);
    --line: oklch(90% 0.01 80 / 0.35);
    --track: oklch(90% 0.01 80 / 0.14);
    --sun-ink: oklch(78% 0.13 78);
    --sage: oklch(72% 0.08 150);
    --rose: oklch(70% 0.06 30);
  }
}
```

  Then set `body { background: var(--sky); background-attachment: fixed; ... }` (keep the rest of the body rule) and remove every remaining `font-weight: 560` / `font-weight: 420` on display text (grep `560\|420` in styles.css; Bricolage has no such axis stops — use `500`).

- [ ] **Step 4: Chrome colours.** `web/index.html`: `<meta name="theme-color" content="#eef0ec">`. `web/public/manifest.webmanifest`: `background_color` and `theme_color` `#eef0ec`. `web/src/theme.ts` `paintChrome()`: read `--earth` instead of `--dawn`.

- [ ] **Step 5: Verify** `cd web && pnpm build` is clean and `pnpm shot today 1440x900 /tmp/t1.png` shows the sky gradient and Bricolage headings with no external request reported. `grep -rn "Fraunces" web/src` returns nothing.

- [ ] **Step 6: Commit**

```bash
git add -A web/src/fonts web/src/styles.css web/index.html web/public/manifest.webmanifest web/src/theme.ts
git commit -m "feat: horizon tokens and self-hosted Bricolage Grotesque"
```

---

### Task 2: Types, API and preferences

**Files:**
- Modify: `web/src/types.ts` (`Settings`, `SettingsSaved`), `web/src/api.ts` (`WRITABLE_SETTINGS`, new calls)
- Create: `web/src/prefs.ts`
- Modify: `web/src/session.ts` (delete `readCounterMode`/`writeCounterMode` and `MODE_KEY`; keep `CounterMode`)
- Modify: `web/src/views/Settings.tsx` (the `AppearanceSection` counter control must compile: switch it to `prefs` — the full Settings rework is Task 8; here only make it compile by importing `readPrefs`/`writePrefs` and replacing the two calls)

**Interfaces:**
- Produces: `Settings.show_arc_between_sessions: boolean`, `Settings.counter: 'remaining' | 'elapsed'`; `api.setEventAlert(id, alert): Promise<void>`; `api.moveTomorrow(id): Promise<{event_id: number; date: string}>`; `prefs.ts`: `type Prefs = { counter: CounterMode; showArc: boolean }`, `readPrefs(): Prefs`, `writePrefs(p: Prefs)`, `prefsFrom(s: Settings): Prefs`.

- [ ] **Step 1: types.ts.** Add to `Settings`: `show_arc_between_sessions: boolean` and `counter: 'remaining' | 'elapsed'`. Change `SettingsSaved` to `Pick<Settings, 'display_name' | 'timezone' | 'nightly_time' | 'template' | 'show_arc_between_sessions' | 'counter'> & { schedule: ScheduleRow[] }`.

- [ ] **Step 2: api.ts.** `WRITABLE_SETTINGS = ['display_name', 'timezone', 'nightly_time', 'template', 'show_arc_between_sessions', 'counter'] as const`. Add to `api`:

```ts
  setEventAlert: (id: number, alert: boolean) =>
    request<void>(`/api/events/${id}/alert`, { method: 'POST', body: JSON.stringify({ alert }) }),
  moveTomorrow: (id: number) =>
    request<{ event_id: number; date: string }>(`/api/events/${id}/move_tomorrow`, { method: 'POST' }),
```

- [ ] **Step 3: prefs.ts:**

```ts
import type { CounterMode } from './session'
import type { Settings } from './types'

const KEY = 'note.prefs'

export type Prefs = { counter: CounterMode; showArc: boolean }

const DEFAULT: Prefs = { counter: 'remaining', showArc: true }

export function readPrefs(): Prefs {
  try {
    const raw = localStorage.getItem(KEY)
    if (!raw) return DEFAULT
    const p = JSON.parse(raw) as Partial<Prefs>
    return {
      counter: p.counter === 'elapsed' ? 'elapsed' : 'remaining',
      showArc: p.showArc !== false,
    }
  } catch {
    return DEFAULT
  }
}

export function writePrefs(p: Prefs) {
  try {
    localStorage.setItem(KEY, JSON.stringify(p))
  } catch {
    // storage blocked; the choice still holds for this session
  }
}

export function prefsFrom(s: Pick<Settings, 'counter' | 'show_arc_between_sessions'>): Prefs {
  return { counter: s.counter, showArc: s.show_arc_between_sessions }
}
```

- [ ] **Step 4: session.ts.** Delete `MODE_KEY`, `readCounterMode`, `writeCounterMode`. In `views/Now.tsx` and `views/Settings.tsx` replace `readCounterMode()` with `readPrefs().counter` and `writeCounterMode(m)` with `writePrefs({ ...readPrefs(), counter: m })` (Now.tsx is deleted in Task 5; this keeps the build green until then).

- [ ] **Step 5: Verify** `pnpm build` clean.

- [ ] **Step 6: Commit**

```bash
git add web/src/types.ts web/src/api.ts web/src/prefs.ts web/src/session.ts web/src/views/Now.tsx web/src/views/Settings.tsx
git commit -m "feat: client types for spans, home settings and the new event routes"
```

---

### Task 3: Shared pieces — Gauge, DayLine, TellNote, stage hook

**Files:**
- Create: `web/src/gauge.tsx`, `web/src/dayline.tsx`, `web/src/tellnote.tsx`, `web/src/stage.ts`
- Modify: `web/src/styles.css` (append a `/* horizon: shared */` section)

**Interfaces:**
- Produces:
  - `Gauge({ size, frac, faded?, children })` — `size` px, `frac` 0..1 (clamped), `faded` draws the whole arc at 38% opacity; children sit centred.
  - `DayLine({ events, now, compact? })` — `now` is minutes since midnight; draws hour ticks, the elapsed line, the now disc, and one span per future event.
  - `TellNote({ placeholder?, notify })` — a text line; Enter posts `api.talk(text, lastConversation())`, clears, toasts `Sent to Note`.
  - `useStage(max: number) → { stage, setStage, bind }` — `bind` is `{ onWheel, onTouchStart, onTouchEnd, onKeyDown, tabIndex }` to spread on the surface. Swipe up / wheel down / ArrowDown raise the stage; swipe down / wheel up / ArrowUp lower it.
  - `lastConversation(): number | undefined` and `rememberConversation(id)` in `tellnote.tsx` (localStorage `note.lastConversation`).

- [ ] **Step 1: gauge.tsx:**

```tsx
import type { ReactNode } from 'react'

// 240° of the ring, open at the bottom; the sweep starts at the lower-left end.
const SWEEP = 240 / 360

export function Gauge({
  size,
  frac,
  faded = false,
  children,
}: {
  size: number
  frac: number
  faded?: boolean
  children?: ReactNode
}) {
  const stroke = Math.max(4, Math.round(size * 0.028))
  const r = size / 2 - stroke * 1.4
  const c = 2 * Math.PI * r
  const full = c * SWEEP
  const prog = full * Math.min(1, Math.max(0, frac))
  const mid = size / 2
  return (
    <div className={`gauge${faded ? ' faded' : ''}`} style={{ width: size, height: size }}>
      <svg className="gauge-ring" viewBox={`0 0 ${size} ${size}`} aria-hidden="true">
        <g transform={`rotate(150 ${mid} ${mid})`}>
          <circle className="gauge-track" cx={mid} cy={mid} r={r} strokeWidth={stroke} strokeDasharray={`${full} ${c}`} />
          {prog > 0 && (
            <circle className="gauge-arc" cx={mid} cy={mid} r={r} strokeWidth={stroke} strokeDasharray={`${prog} ${c}`} />
          )}
        </g>
      </svg>
      <div className="gauge-centre">{children}</div>
    </div>
  )
}
```

- [ ] **Step 2: dayline.tsx:**

```tsx
import { eventLabel } from './receipts'
import type { PlanEvent } from './types'

const START = 6 * 60
const END = 24 * 60

export function minutesOf(wall: string): number {
  const [h, m] = wall.split(':')
  return Number(h) * 60 + Number(m)
}

const pct = (mins: number) => `${((Math.min(END, Math.max(START, mins)) - START) / (END - START)) * 100}%`

// Only what is ahead is drawn; the solid line behind the disc is all the past needs.
export function DayLine({
  events,
  now,
  compact = false,
}: {
  events: PlanEvent[]
  now: number
  compact?: boolean
}) {
  const ahead = events.filter(
    (ev) =>
      (ev.status === 'pending' || ev.status === 'snoozed' || ev.status === 'fired') &&
      minutesOf(ev.end_wall_time ?? ev.wall_time) >= now,
  )
  const nextId = ahead[0]?.id
  const hours = compact ? [6, 15, 24] : [6, 9, 12, 15, 18, 21, 24]
  return (
    <div className={`dayline${compact ? ' compact' : ''}`} role="img" aria-label="Today, drawn as a line">
      <span className="dl-line" />
      <span className="dl-gone" style={{ width: pct(now) }} />
      <span className="dl-ticks" />
      {hours.map((h) => (
        <span key={h} className="dl-hour" style={{ left: pct(h * 60) }}>
          {String(h).padStart(2, '0')}
        </span>
      ))}
      {ahead.map((ev) => {
        const a = minutesOf(ev.wall_time)
        const b = minutesOf(ev.end_wall_time ?? ev.wall_time)
        const w = Math.max(1, ((b - a) / (END - START)) * 100)
        return (
          <span key={ev.id} className={`dl-span${ev.id === nextId ? ' next' : ''}`} style={{ left: pct(a), width: `${w}%` }}>
            {!compact && (
              <span className={`dl-label${ev.id === nextId ? ' below' : ' above'}`}>
                {ev.wall_time} – {ev.end_wall_time ?? ev.wall_time} {eventLabel(ev.kind)}
              </span>
            )}
          </span>
        )
      })}
      <span className="dl-now" style={{ left: pct(now) }} />
    </div>
  )
}
```

- [ ] **Step 3: tellnote.tsx:**

```tsx
import { useState, type FormEvent } from 'react'
import { api } from './api'
import type { ToastAction } from './app'

const KEY = 'note.lastConversation'

export function lastConversation(): number | undefined {
  try {
    const raw = localStorage.getItem(KEY)
    return raw ? Number(raw) : undefined
  } catch {
    return undefined
  }
}

export function rememberConversation(id: number) {
  try {
    localStorage.setItem(KEY, String(id))
  } catch {
    // storage blocked; the next message starts a new thread
  }
}

export function TellNote({
  placeholder = 'Tell Note',
  notify,
  onSent,
}: {
  placeholder?: string
  notify: (msg: string, action?: ToastAction) => void
  onSent?: () => void
}) {
  const [text, setText] = useState('')
  const [busy, setBusy] = useState(false)
  const submit = async (e: FormEvent) => {
    e.preventDefault()
    const message = text.trim()
    if (!message || busy) return
    setBusy(true)
    try {
      const reply = await api.talk(message, lastConversation())
      rememberConversation(reply.conversation_id)
      setText('')
      notify(reply.reply.length > 90 ? `${reply.reply.slice(0, 88)}…` : reply.reply)
      onSent?.()
    } catch {
      notify("Couldn't reach Note. Try again.")
    } finally {
      setBusy(false)
    }
  }
  return (
    <form className="tellnote" onSubmit={submit}>
      <input value={text} placeholder={placeholder} aria-label={placeholder} onChange={(e) => setText(e.target.value)} />
      <button type="submit" aria-label="Send" disabled={busy || !text.trim()}>
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12h14" /><path d="M13 6l6 6-6 6" /></svg>
      </button>
    </form>
  )
}
```

- [ ] **Step 4: stage.ts:**

```ts
import { useCallback, useRef, useState, type KeyboardEvent, type TouchEvent, type WheelEvent } from 'react'

const SWIPE_PX = 40
const WHEEL_PX = 60
const COOLDOWN_MS = 500

// A surface that reveals itself in steps: swipe up, wheel down or ArrowDown
// go one step further; the opposite gestures come back.
export function useStage(max: number) {
  const [stage, setStage] = useState(0)
  const touchY = useRef<number | null>(null)
  const wheel = useRef(0)
  const lastAt = useRef(0)

  const step = useCallback(
    (delta: 1 | -1) => {
      const at = Date.now()
      if (at - lastAt.current < COOLDOWN_MS) return
      lastAt.current = at
      setStage((s) => Math.min(max, Math.max(0, s + delta)))
    },
    [max],
  )

  const onWheel = (e: WheelEvent) => {
    wheel.current += e.deltaY
    if (wheel.current > WHEEL_PX) {
      wheel.current = 0
      step(1)
    } else if (wheel.current < -WHEEL_PX) {
      wheel.current = 0
      step(-1)
    }
  }
  const onTouchStart = (e: TouchEvent) => {
    touchY.current = e.touches[0]?.clientY ?? null
  }
  const onTouchEnd = (e: TouchEvent) => {
    const from = touchY.current
    touchY.current = null
    const to = e.changedTouches[0]?.clientY
    if (from === null || to === undefined) return
    if (from - to > SWIPE_PX) step(1)
    else if (to - from > SWIPE_PX) step(-1)
  }
  const onKeyDown = (e: KeyboardEvent) => {
    const el = e.target as HTMLElement | null
    if (el?.tagName === 'INPUT' || el?.tagName === 'TEXTAREA') return
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      step(1)
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      step(-1)
    }
  }

  return { stage, setStage, bind: { onWheel, onTouchStart, onTouchEnd, onKeyDown, tabIndex: -1 } }
}
```

- [ ] **Step 5: CSS.** Append to `web/src/styles.css`:

```css
/* ── horizon: shared ─────────────────────────────────────────────── */
.gauge { position: relative; flex: none; }
.gauge-ring { width: 100%; height: 100%; overflow: visible; display: block; }
.gauge-track, .gauge-arc { fill: none; stroke-linecap: round; }
.gauge-track { stroke: var(--track); }
.gauge-arc { stroke: var(--sun); transition: stroke-dasharray 900ms linear; }
.gauge.faded .gauge-ring { opacity: 0.38; }
.gauge-ring { animation: gauge-breathe 7s ease-in-out infinite; }
.gauge.faded .gauge-ring, .gauge.still .gauge-ring { animation: none; }
@keyframes gauge-breathe { 0%, 100% { opacity: 0.55; } 50% { opacity: 1; } }
@media (prefers-reduced-motion: reduce) { .gauge-ring { animation: none; } }
.gauge-centre {
  position: absolute; inset: 0;
  display: flex; flex-direction: column; align-items: center; justify-content: center;
  text-align: center; gap: 4px; padding: 0 14%;
}
.gauge-num { font-family: var(--display); font-weight: 500; letter-spacing: -0.035em; line-height: 1; color: var(--ink); font-variant-numeric: tabular-nums; }
.gauge-num.over { color: var(--sun-ink); }
.gauge-name { margin-top: 6px; font-family: var(--display); font-weight: 500; letter-spacing: -0.015em; line-height: 1.2; color: var(--quiet); text-wrap: balance; }
.gauge-sub { font-size: 0.75rem; color: var(--faint); font-variant-numeric: tabular-nums; }
.gauge-eyebrow { font-size: 0.78rem; letter-spacing: 0.12em; font-weight: 700; color: var(--sun-ink); margin-bottom: 6px; }

.dayline { position: relative; height: 0; width: 100%; }
.dl-line { position: absolute; left: 0; right: 0; top: 0; height: 1px; background: var(--line); }
.dl-gone { position: absolute; left: 0; top: -1px; height: 3px; border-radius: 2px; background: var(--ink); opacity: 0.7; }
.dl-ticks { position: absolute; left: 0; right: 0; top: 4px; height: 6px; background: repeating-linear-gradient(to right, var(--line) 0 1px, transparent 1px calc(100% / 18)); }
.dl-hour { position: absolute; top: 34px; transform: translateX(-50%); font-size: 0.72rem; letter-spacing: 0.06em; color: var(--faint); font-variant-numeric: tabular-nums; }
.dl-hour:first-of-type { transform: none; }
.dl-hour:last-of-type { transform: translateX(-100%); }
.dl-now { position: absolute; top: 0; transform: translate(-50%, -50%); width: 14px; height: 14px; border-radius: 50%; background: var(--ink); }
.dl-span { position: absolute; top: -6px; height: 12px; min-width: 10px; border-radius: 6px; border: 1.5px solid var(--quiet); box-sizing: border-box; background: var(--earth); }
.dl-span.next { background: var(--ink); border-color: var(--ink); }
.dl-label { position: absolute; left: 50%; transform: translateX(-50%); white-space: nowrap; font-size: 0.78rem; color: var(--quiet); font-variant-numeric: tabular-nums; }
.dl-label.above { bottom: 18px; }
.dl-label.below { top: 18px; color: var(--ink); font-weight: 700; }
.dayline.compact .dl-hour { top: 14px; font-size: 0.66rem; }
.dayline.compact .dl-ticks { height: 5px; }
.dayline.compact .dl-now { width: 12px; height: 12px; }
.dayline.compact .dl-span { top: -5px; height: 10px; min-width: 8px; }

.tellnote { display: flex; align-items: center; gap: 10px; padding: 0 6px 0 16px; border-radius: 999px; background: var(--haze-strong); min-height: 48px; }
.tellnote input { flex: 1; min-width: 0; border: 0; background: none; font: inherit; font-size: 0.92rem; color: var(--ink); outline: none; }
.tellnote input::placeholder { color: var(--faint); }
.tellnote button { width: 36px; height: 36px; border: 0; border-radius: 50%; background: none; color: var(--quiet); cursor: pointer; display: grid; place-items: center; }
.tellnote button:disabled { color: var(--faint); cursor: default; }
.tellnote button svg { width: 18px; height: 18px; fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round; }
.tellnote button:focus-visible, .tellnote input:focus-visible { outline: 2px solid var(--ring); outline-offset: 2px; }

.chev { display: flex; align-items: center; justify-content: center; height: 38px; color: var(--faint); background: none; border: 0; width: 100%; cursor: pointer; }
.chev svg { width: 18px; height: 18px; fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round; }

.btn-fill { font: inherit; font-weight: 700; font-size: 1.0625rem; line-height: 1.2; padding: 15px 34px; border: 0; border-radius: 999px; background: var(--ink); color: var(--ivory); cursor: pointer; box-shadow: 0 12px 28px -14px oklch(23% 0.03 255 / 0.7); }
.btn-haze { font: inherit; font-size: 1.0625rem; line-height: 1.2; padding: 14px 24px; border: 0; border-radius: 999px; background: var(--haze-strong); color: var(--ink); cursor: pointer; }
.btn-fill:disabled, .btn-haze:disabled { opacity: 0.5; cursor: default; }
.btn-fill:focus-visible, .btn-haze:focus-visible { outline: 2px solid var(--ring); outline-offset: 2px; }
.btn-round { width: 44px; height: 44px; border: 0; border-radius: 50%; background: var(--haze-strong); color: var(--quiet); display: grid; place-items: center; cursor: pointer; }
.btn-round svg { width: 18px; height: 18px; fill: currentColor; }
```

- [ ] **Step 6: Verify** `pnpm build` clean (the new files are unused yet; `noUnusedLocals` does not flag unused exports).

- [ ] **Step 7: Commit**

```bash
git add web/src/gauge.tsx web/src/dayline.tsx web/src/tellnote.tsx web/src/stage.ts web/src/styles.css
git commit -m "feat: gauge, day line, tell-note line and stage hook"
```

---

### Task 4: Home

**Files:**
- Create: `web/src/views/Home.tsx`
- Modify: `web/src/styles.css` (append `/* horizon: home */`)

**Interfaces:**
- Consumes: `Gauge`, `DayLine`, `minutesOf`, `TellNote`, `useStage`, `readPrefs`, `NowCounter` (existing, `nowcounter.tsx`), `FocusSession`, `elapsedSec`, `effectiveStart`, `api.planToday/eventAction/snooze/setEventAlert/moveTomorrow/patchTask`, `Overflow` (existing `overflow.tsx`, props `{label, items: {label, run, disabled?}[], className?}`), `eventLabel`.
- Produces: `Home({ session, setSession, notify, onChanged, refresh, openNow, mobile, onChrome, tabs })` — `mobile` selects the phone layout; `onChrome(hidden)` tells the shell to hide its tabs/nav; `tabs` is the shell's tab bar node to render at stage 2 on mobile.

- [ ] **Step 1: Write `web/src/views/Home.tsx`:**

```tsx
import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react'
import { api, ApiError } from '../api'
import type { ToastAction } from '../app'
import { DayLine, minutesOf } from '../dayline'
import { Gauge } from '../gauge'
import { NowCounter } from '../nowcounter'
import { Overflow } from '../overflow'
import { readPrefs } from '../prefs'
import { eventLabel } from '../receipts'
import { effectiveStart, elapsedSec, type FocusSession } from '../session'
import { useStage } from '../stage'
import { TellNote } from '../tellnote'
import type { PlanEvent } from '../types'

const UNDO_MS = 5000
const LATER_MINUTES = [5, 10, 15, 30, 60]

// Drop has no server-side reversal, so the request waits out the undo window.
let heldDrop: { id: number; timer: number } | null = null

function nowMinutes(): number {
  const d = new Date()
  return d.getHours() * 60 + d.getMinutes()
}

function fmt(sec: number): string {
  const s = Math.max(0, sec)
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`
}

function actionMessage(err: unknown): string {
  if (err instanceof ApiError) {
    if (err.status === 409) return 'Already settled.'
    if (err.status === 404) return 'That event is gone.'
  }
  return 'Something went wrong. Try again.'
}

// The fired event owns the face; failing that, the next routine still open does.
function nextUp(events: PlanEvent[]): PlanEvent | null {
  return (
    events.find((ev) => ev.status === 'fired') ??
    events.find((ev) => ev.entry !== 'block' && (ev.status === 'pending' || ev.status === 'snoozed')) ??
    null
  )
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
  const [, tick] = useState(0)
  const [pending, setPending] = useState(false)
  const [later, setLater] = useState(false)
  const inSession = session !== null
  const { stage, setStage, bind } = useStage(inSession ? 2 : 1)
  const prefs = readPrefs()

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
  useEffect(() => onChrome(mobile && !showToday), [mobile, showToday, onChrome])
  useEffect(() => setStage(0), [inSession, setStage])

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

  const visible = useMemo(() => events.filter((ev) => ev.id !== heldDrop?.id), [events])
  const now = nowMinutes()
  const next = nextUp(visible)

  const start = (ev: PlanEvent) =>
    openNow({
      taskId: null,
      eventId: ev.id,
      title: eventLabel(ev.kind),
      notes: '',
      stepIndex: null,
      stepCount: null,
      stepName: null,
      durationSec: Math.max(60, (minutesOf(ev.end_wall_time ?? ev.wall_time) - minutesOf(ev.wall_time)) * 60),
      startedAt: Date.now(),
      pausedAt: null,
      pausedMs: 0,
    })

  // ── session face ─────────────────────────────────────────────
  const pause = () => session && setSession({ ...session, pausedAt: Date.now() })
  const resume = () =>
    session &&
    setSession({
      ...session,
      pausedAt: null,
      pausedMs: session.pausedMs + (Date.now() - (session.pausedAt ?? Date.now())),
    })
  const finish = () => {
    if (!session) return
    const elapsed = elapsedSec(session)
    const done = () => {
      setSession(null)
      onChanged()
    }
    if (session.taskId !== null) {
      api
        .patchTask(session.taskId, { state: 'done', notes: withElapsedNote(session.notes, elapsed) })
        .then(done)
        .catch(() => notify("Couldn't save the session. Try again."))
    } else if (session.eventId !== null) {
      api
        .eventAction(session.eventId, 'done')
        .then(done)
        .catch(() => notify("Couldn't mark that done. Try again."))
    } else {
      done()
    }
  }

  const sessionCentre = (session: FocusSession, big: boolean) => {
    const elapsed = elapsedSec(session)
    const total = session.durationSec
    const over = total !== null && elapsed > total
    const shown = total === null || prefs.counter === 'elapsed' ? elapsed : over ? elapsed - total : total - elapsed
    return (
      <>
        <div className={`gauge-num${over ? ' over' : ''}`} style={{ fontSize: big ? 58 : 40 }}>
          {over ? '+' : ''}
          {big ? (
            <NowCounter startedAt={effectiveStart(session)} durationSec={total ?? 0} mode={total === null ? 'elapsed' : prefs.counter} pausedAt={session.pausedAt} />
          ) : (
            fmt(shown)
          )}
        </div>
        <div className="gauge-name" style={{ fontSize: big ? 18 : 15 }}>{session.stepName ?? session.title}</div>
        {session.stepIndex !== null && (
          <div className="gauge-sub">{session.stepIndex} of {session.stepCount}</div>
        )}
      </>
    )
  }

  const sessionFrac = (session: FocusSession) =>
    session.durationSec ? elapsedSec(session) / session.durationSec : 0

  // ── wait face ────────────────────────────────────────────────
  const waitCentre = (ev: PlanEvent, big: boolean) => {
    const mins = Math.max(0, minutesOf(ev.wall_time) - now)
    return (
      <>
        <div className="gauge-eyebrow">NEXT</div>
        <div className="gauge-num" style={{ fontSize: big ? 50 : 22 }}>{mins} min</div>
        {big && (
          <>
            <div className="gauge-name" style={{ fontSize: 18 }}>{eventLabel(ev.kind)}</div>
            <div className="gauge-sub">{ev.wall_time} – {ev.end_wall_time ?? ev.wall_time}</div>
          </>
        )}
      </>
    )
  }
  const waitFrac = (ev: PlanEvent) => {
    const from = waitStart(visible, now)
    const to = minutesOf(ev.wall_time)
    return to <= from ? 1 : (now - from) / (to - from)
  }

  const nextActions = (ev: PlanEvent) => (
    <div className="home-actions">
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
    </div>
  )

  const chevron = (
    <button className="chev" aria-label="More" onClick={() => setStage((s) => Math.min(inSession ? 2 : 1, s + 1))}>
      <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 14l6-6 6 6" /></svg>
    </button>
  )

  const today = (
    <div className="home-today">
      <DayLine events={visible} now={now} compact={mobile} />
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
    </div>
  )

  // ── layout ───────────────────────────────────────────────────
  if (session) {
    const big = stage === 0
    return (
      <div className={`home in-session stage-${stage}${mobile ? ' mobile' : ''}`} {...bind}>
        <div className="home-face">
          <Gauge size={stage === 2 ? 120 : big ? (mobile ? 320 : 440) : 230} frac={sessionFrac(session)}>
            {stage === 2 ? <div className="gauge-num" style={{ fontSize: 24 }}>{fmt(session.durationSec === null || prefs.counter === 'elapsed' ? elapsedSec(session) : Math.abs(session.durationSec - elapsedSec(session)))}</div> : sessionCentre(session, big)}
          </Gauge>
          {stage === 2 && (
            <div className="home-head">
              <span className="home-head-name">{session.stepName ?? session.title}</span>
              {session.stepIndex !== null && <span className="gauge-sub">{session.stepIndex} of {session.stepCount}</span>}
            </div>
          )}
        </div>
        {stage === 1 && (
          <div className="home-sheet">
            <button className="btn-round" aria-label={session.pausedAt ? 'Back to it' : 'Break'} onClick={session.pausedAt ? resume : pause}>
              {session.pausedAt ? (
                <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 5.5v13l10-6.5z" /></svg>
              ) : (
                <svg viewBox="0 0 24 24" aria-hidden="true"><rect x="6" y="5" width="4" height="14" rx="1.2" /><rect x="14" y="5" width="4" height="14" rx="1.2" /></svg>
              )}
            </button>
            <button className="btn-fill wide" onClick={finish}>Done with this step</button>
            <TellNote notify={notify} />
          </div>
        )}
        {stage === 2 && today}
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
            <div className="home-text">
              <div className="gauge-eyebrow">NEXT</div>
              <div className="home-title">{eventLabel(next.kind)}</div>
              <div className="gauge-num" style={{ fontSize: 30 }}>in {Math.max(0, minutesOf(next.wall_time) - now)} min</div>
              <div className="gauge-sub">{next.wall_time} – {next.end_wall_time ?? next.wall_time}</div>
            </div>
          )
        ) : (
          <div className="home-text"><div className="home-title">That's everything today.</div></div>
        )}
        {stage === 1 && next && (
          <div className="home-head">
            <span className="gauge-eyebrow">NEXT</span>
            <span className="home-head-name">{eventLabel(next.kind)}</span>
            <span className="gauge-sub">{next.wall_time} – {next.end_wall_time ?? next.wall_time}</span>
            <button className="btn-fill small" disabled={pending} onClick={() => start(next)}>Start</button>
          </div>
        )}
      </div>
      {stage === 0 && next && nextActions(next)}
      {stage === 1 && today}
      {stage === 1 && mobile && tabs}
      {stage === 0 && chevron}
    </div>
  )
}
```

  Read `web/src/overflow.tsx` before using `Overflow` and match its real prop names; read `web/src/nowcounter.tsx` for `NowCounter`'s props (`startedAt, durationSec, mode, pausedAt`) — pass them exactly. If `NowCounter` renders its own `+` for overrun, drop the `{over ? '+' : ''}` prefix in the big branch.

- [ ] **Step 2: CSS.** Append to `styles.css`:

```css
/* ── horizon: home ───────────────────────────────────────────────── */
.home { position: relative; min-height: 100dvh; display: flex; flex-direction: column; align-items: center; outline: none; background: var(--sky); background-attachment: fixed; }
.home-face { display: flex; flex-direction: column; align-items: center; width: 100%; padding-top: 200px; transition: padding-top 400ms ease; }
.home.stage-1 .home-face, .home.stage-2 .home-face { padding-top: 96px; }
.home.stage-2 .home-face, .home:not(.in-session).stage-1 .home-face { flex-direction: row; align-items: center; gap: 16px; padding: 54px 24px 0; box-sizing: border-box; }
.home-head { display: flex; flex-direction: column; gap: 2px; min-width: 0; flex: 1; }
.home-head-name { font-family: var(--display); font-weight: 500; font-size: 1.0625rem; letter-spacing: -0.015em; color: var(--ink); }
.home-actions { position: relative; display: flex; align-items: center; gap: 10px; margin-top: 50px; }
.home-actions .ev-more-wrap { margin-left: 6px; }
.later-pick { position: absolute; left: 0; top: calc(100% + 14px); width: min(400px, 92vw); padding: 14px 16px; border-radius: 18px; background: var(--haze-strong); box-shadow: 0 24px 50px -30px oklch(23% 0.03 255 / 0.5); display: flex; flex-wrap: wrap; align-items: center; gap: 8px; z-index: 5; }
.later-lead { width: 100%; font-size: 0.8125rem; color: var(--quiet); }
.later-min { flex: 1; font: inherit; font-variant-numeric: tabular-nums; padding: 9px 0; border: 0; border-radius: 999px; background: var(--earth); color: var(--ink); cursor: pointer; }
.later-min:hover, .later-min:focus-visible { background: var(--ink); color: var(--ivory); outline: none; }
.later-unit { flex: 1; text-align: center; font-size: 0.8125rem; color: var(--quiet); }
.home-sheet { display: flex; flex-direction: column; align-items: center; gap: 22px; width: min(342px, 88vw); margin-top: 40px; }
.home-sheet .tellnote { width: 100%; box-sizing: border-box; }
.btn-fill.wide { width: 100%; }
.btn-fill.small { padding: 10px 18px; font-size: 0.9375rem; box-shadow: none; }
.home-today { width: min(342px, 88vw); margin-top: 60px; display: flex; flex-direction: column; gap: 40px; }
.home:not(.mobile) .home-today { width: min(1200px, 84vw); }
.home-list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; }
.home-list li { display: grid; grid-template-columns: 96px minmax(0, 1fr); align-items: center; gap: 12px; min-height: 44px; color: var(--quiet); }
.home-list li.next { color: var(--ink); font-weight: 700; }
.home-when { font-size: 0.8125rem; color: var(--faint); font-variant-numeric: tabular-nums; font-weight: 400; }
.home-text { display: flex; flex-direction: column; gap: 10px; padding: 0 28px; width: 100%; box-sizing: border-box; }
.home-title { font-family: var(--display); font-weight: 500; font-size: 2.75rem; letter-spacing: -0.03em; line-height: 1.02; color: var(--ink); text-wrap: balance; }
.home .chev { position: absolute; left: 0; right: 0; bottom: 0; }
.home.mobile .tabs { position: sticky; bottom: 0; width: 100%; }
```

- [ ] **Step 3: Verify** `pnpm build` clean. (Home is not mounted yet; Task 5 wires it.)

- [ ] **Step 4: Commit**

```bash
git add web/src/views/Home.tsx web/src/styles.css
git commit -m "feat: the home screen, in and between sessions"
```

---

### Task 5: Shell — desktop nav, mobile Home, no Now takeover

**Files:**
- Modify: `web/src/app.tsx`
- Delete: `web/src/views/Now.tsx`
- Modify: `web/src/styles.css` (shell section :152-310 replaced; the `.now-*` section :2024 to the end of the Now sheet rules deleted; `.now-counter` rules at :1992-2023 kept — Home uses `NowCounter`)

**Interfaces:**
- Consumes: `Home` (Task 4), `readPrefs/writePrefs/prefsFrom` (Task 2).
- Produces: `ViewProps` unchanged; `App` renders `<Home>` for the Today tab on mobile always, and on desktop while a session is active; `<Today>` (Task 6) on desktop otherwise.

- [ ] **Step 1: Rewrite the shell in `web/src/app.tsx`.** Keep `Capture`, `Login`, `ToastAction`, `ViewProps`, the undo hold, the WebSocket effect and `notify` exactly as they are. Replace `App`'s render tail (from `// The Now screen is a full-bleed…` to the end of the component) with:

```tsx
  const tabsNode = (
    <nav className={`tabs${chromeHidden ? ' hidden' : ''}`} aria-label="Views">
      {NAV.map((t) => (
        <button key={t.id} aria-current={tab === t.id} onClick={() => setTab(t.id)}>
          <NavIcon id={t.id} />
          {t.label}
        </button>
      ))}
    </nav>
  )

  const home = (
    <Home
      session={session}
      setSession={changeSession}
      notify={notify}
      onChanged={onChanged}
      refresh={refresh}
      openNow={changeSession}
      mobile={mobile}
      onChrome={setChromeHidden}
      tabs={tabsNode}
    />
  )

  const showHome = tab === 'today' && (mobile || session !== null)

  return (
    <div className={`shell${chromeHidden ? ' bare' : ''}`}>
      {!mobile && (
        <header className="topbar">
          <span className="brand">Note</span>
          <nav className="topnav" aria-label="Views">
            {NAV.map((t) => (
              <button key={t.id} aria-current={tab === t.id} onClick={() => setTab(t.id)}>
                {t.label}
              </button>
            ))}
          </nav>
          <Capture notify={notify} onChanged={onChanged} />
        </header>
      )}
      <main className={`view${tab === 'chat' ? ' view-talk' : ''}${showHome ? ' view-home' : ''}`}>
        {showHome && home}
        {tab === 'today' && !showHome && <Today key={refresh} {...views} />}
        {tab === 'tasks' && <Tasks {...views} />}
        {tab === 'chat' && (
          <Talk {...views} prefill={talkPrefill} onPrefilled={() => setTalkPrefill(null)} />
        )}
        {tab === 'memory' && <Memory {...views} />}
        {tab === 'settings' && <Settings me={me} {...views} onSignedOut={() => setMe(null)} />}
      </main>
      {mobile && !showHome && tabsNode}
      {toastNode}
    </div>
  )
```

  Add state and the media hook near the top of `App`:

```tsx
  const [chromeHidden, setChromeHidden] = useState(false)
  const mobile = useMedia('(max-width: 767.98px)')
```

  and the hook at module level:

```tsx
function useMedia(query: string): boolean {
  const [matches, setMatches] = useState(() => window.matchMedia(query).matches)
  useEffect(() => {
    const mq = window.matchMedia(query)
    const on = () => setMatches(mq.matches)
    mq.addEventListener('change', on)
    return () => mq.removeEventListener('change', on)
  }, [query])
  return matches
}
```

  Import `Home` from `./views/Home`, drop the `Now` import, and delete `web/src/views/Now.tsx`. After login (`setMe` resolves), load prefs once:

```tsx
  useEffect(() => {
    if (!me) return
    api
      .settings()
      .then((s) => writePrefs(prefsFrom(s)))
      .catch(() => {})
  }, [me])
```

  (import `writePrefs, prefsFrom` from `./prefs`). Remove the `.brand-glyph` spans and the `.mobile-head` block entirely; `Capture`'s placeholder becomes `'Jot anything'`.

- [ ] **Step 2: CSS.** Replace the shell rules (`.shell` … through the `.tabs` media block that ends near :310) with:

```css
.shell { display: flex; flex-direction: column; min-height: 100dvh; }
.topbar { display: flex; align-items: center; gap: 28px; height: 64px; padding: 0 48px; }
.brand { font-family: var(--display); font-weight: 600; font-size: 1.375rem; letter-spacing: -0.01em; color: var(--ink); margin: 0; }
.topnav { display: flex; align-items: center; gap: 2px; }
.topnav button { font: inherit; font-size: 0.9rem; line-height: 1.2; padding: 8px 16px; border: 0; border-radius: 999px; background: none; color: var(--quiet); cursor: pointer; }
.topnav button[aria-current='true'] { background: var(--haze-strong); color: var(--ink); font-weight: 700; }
.topnav button:focus-visible { outline: 2px solid var(--ring); outline-offset: 2px; }
.topbar .capture { margin-left: auto; width: 420px; }
.view { flex: 1; display: flex; flex-direction: column; }
.view-home { padding: 0; }
.tabs { display: flex; height: 84px; padding: 12px 18px 22px; box-sizing: border-box; background: var(--haze-strong); }
.tabs.hidden { display: none; }
.tabs button { flex: 1; display: flex; flex-direction: column; align-items: center; gap: 4px; font: inherit; font-size: 0.66rem; border: 0; background: none; color: var(--faint); cursor: pointer; }
.tabs button[aria-current='true'] { color: var(--ink); }
.tabs .nav-icon { width: 22px; height: 22px; }
.tabs button:focus-visible { outline: 2px solid var(--ring); outline-offset: -2px; }
@media (max-width: 767.98px) { .topbar { display: none; } }
@media (min-width: 768px) { .tabs { display: none; } }
```

  Then delete the old `.sidebar*`, `.mobile-head*`, `.now-screen` … `.now-sheet-item` rules (keep `.now-counter*`). Restyle `.capture` to the haze pill: `background: var(--haze-strong); border: 0; border-radius: 999px; min-height: 40px;` with `.capture-glyph` as the `+` in `--faint` and `.capture-key` a small bordered `N`.

- [ ] **Step 3: Verify.** `pnpm build` clean. Screenshots: `pnpm shot today 390x844 /tmp/home.png` (wait face: faded arc, NEXT, Start/Later/dots, chevron, no tabs), `pnpm shot today 390x844 /tmp/session.png --session` (gauge face), `pnpm shot today 1440x900 /tmp/desk-session.png --session` (desktop session face). Compare with `docs/superpowers/mockups/horizon/HomeIdle.dc.html`, `Session.dc.html`, `SessionDesktop.dc.html`. Fix spacing until they match within reason.

- [ ] **Step 4: Commit**

```bash
git add -A web/src
git commit -m "feat: home replaces the now takeover; desktop top nav"
```

---

### Task 6: Today, the desktop page

**Files:**
- Rewrite: `web/src/views/Today.tsx`
- Modify: `web/src/styles.css` (delete the `.today .spine .ev* .now-line .now-rule .now-dot .now-label .nowcard* .today-clear .today-tomorrow` rules; keep `.debrief-*` and `.letter`; append `/* horizon: today */`)

**Interfaces:**
- Consumes: `DayLine`, `minutesOf`, `Overflow`, `api.*`, `eventLabel`, `DebriefFold` (keep the existing function and its helpers from the current file).

- [ ] **Step 1: Rewrite `Today.tsx`.** Keep `DebriefFold`, `readFold`, `writeFold`, `firstSentence`, `FOLD_KEY` verbatim, but change the fold's glyph: replace `☀︎` with `<span className="debrief-mark" aria-hidden="true" />` and `▾`/`▴` with an inline chevron SVG (`<svg viewBox="0 0 24 24"><path d="M6 9l6 6 6-6"/></svg>`, rotated by CSS when open). Replace everything else with:

```tsx
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

function nextUp(events: PlanEvent[]): PlanEvent | null {
  return (
    events.find((ev) => ev.status === 'fired') ??
    events.find((ev) => ev.entry !== 'block' && (ev.status === 'pending' || ev.status === 'snoozed')) ??
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
  const next = nextUp(visible)
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
```

  (`Debrief` stays imported for `DebriefFold`.)

- [ ] **Step 2: CSS.** Append:

```css
/* ── horizon: today (desktop) ───────────────────────────────────── */
.today { position: relative; min-height: calc(100dvh - 64px); }
.today-hero { position: relative; padding: 86px 0 0 120px; width: 820px; display: flex; flex-direction: column; gap: 14px; }
.today-eyebrow { display: flex; align-items: center; gap: 10px; font-size: 0.8125rem; letter-spacing: 0.1em; font-weight: 700; color: var(--sun-ink); font-variant-numeric: tabular-nums; }
.today-dot { width: 4px; height: 4px; border-radius: 50%; background: var(--sun-ink); opacity: 0.6; }
.today-title { margin: 0; font-family: var(--display); font-weight: 500; font-size: 4.5rem; letter-spacing: -0.025em; line-height: 1.02; color: var(--ink); }
.today-when { display: flex; align-items: baseline; gap: 14px; margin-top: 4px; }
.today-in { font-family: var(--display); font-weight: 500; font-size: 1.875rem; letter-spacing: -0.02em; color: var(--ink); font-variant-numeric: tabular-nums; }
.today-span { font-size: 0.9375rem; color: var(--quiet); font-variant-numeric: tabular-nums; }
.today-actions { position: relative; display: flex; align-items: center; gap: 12px; margin-top: 18px; }
.today-line { position: absolute; left: 120px; width: 1200px; max-width: calc(100% - 240px); top: 496px; }
.today-ground { position: absolute; left: 0; right: 0; top: 560px; bottom: 0; background: var(--earth); padding: 100px 0 0 120px; }
.today-ground .debrief-row { width: 820px; max-width: calc(100% - 240px); }
.debrief-fold { background: var(--haze-strong); border: 0; border-radius: 16px; }
.debrief-mark { width: 18px; height: 18px; border-radius: 50%; border: 1.7px solid var(--sun-ink); box-sizing: border-box; flex: none; }
.debrief-chev svg { width: 16px; height: 16px; fill: none; stroke: var(--faint); stroke-width: 1.8; transition: transform 200ms; }
.debrief-fold[aria-expanded='true'] .debrief-chev svg { transform: rotate(180deg); }
```

  Delete the old Today rules (`.today .page` spacing, `.spine`, `.ev*`, `.now-line`, `.now-rule`, `.now-dot`, `.now-label`, `.nowcard*`, `.actions-spacer`, `.today-clear`, `.today-tomorrow`, and the `.bell` rules if `Bell` is no longer imported anywhere — `grep -rn "from '../bell'" web/src`; delete `bell.tsx` when unused).

- [ ] **Step 3: Verify.** `pnpm build`; `pnpm shot today 1440x900 /tmp/today.png` against `Main.dc.html`. The hero, the line with only future spans, the folded letter on the earth band.

- [ ] **Step 4: Commit**

```bash
git add -A web/src
git commit -m "feat: today as a hero and a day line"
```

---

### Task 7: Tasks

**Files:**
- Modify: `web/src/views/Tasks.tsx` (markup only; keep every helper and state), `web/src/styles.css` (`.task*` rules)

**Interfaces:**
- Consumes: existing `groups()`, `Row`, `Duration`, `startFocus`, `Overflow`.

- [ ] **Step 1: Markup.** In `Tasks.tsx`:
  - The add form: `<form className="task-add tellnote">` with the input placeholder `Add a task` and the submit button carrying the arrow SVG from `TellNote` (copy the `<button type="submit">…</button>` markup); remove `.task-add-glyph` and `.task-add-hint`.
  - Group heads: `NOW` (no count, no `.task-group-why`), `LATER · n`, and the done group becomes one folded row: `<button className="task-done-fold" aria-expanded={showDone} onClick={() => setShowDone(v => !v)}>Done today · {n} <svg …chevron…/></button>` followed by the rows only when `showDone` (new `useState(false)`).
  - `Row`: the Start control becomes `<button className="task-start" aria-label="Start">` containing a play SVG (`<svg viewBox="0 0 24 24"><path d="M9 7.5v9l7-4.5z"/></svg>`), rendered only in the Now group; `Later` rows show `Overflow` with items `Move to Now`, `Drop` (existing handlers; if there is no drop handler for tasks, use `patchTask(id, { state: 'dropped' })` with the existing undo `Snapshot` mechanism the file already uses for state changes).
  - Remove the empty-state sentences `NOW_FULL` toast stays; `NOW_EMPTY` text is deleted (an empty Now group renders nothing).
  - Steps: keep `.task-steps`; each step row shows the minute count as plain faint text at the right.

- [ ] **Step 2: CSS.** Replace the `.task*` rules with:

```css
/* ── horizon: tasks ─────────────────────────────────────────────── */
.tasks { padding: 56px 24px 100px; max-width: 720px; margin: 0 auto; }
@media (min-width: 768px) { .tasks { margin: 0; padding: 46px 0 0 120px; } }
.task-add { margin-bottom: 26px; }
.task-group { display: flex; flex-direction: column; gap: 6px; margin-top: 28px; }
.task-group:first-of-type { margin-top: 0; }
.task-group-head { margin: 0; font-size: 0.72rem; letter-spacing: 0.12em; font-weight: 700; color: var(--faint); }
.task-row { display: flex; align-items: center; gap: 12px; min-height: 52px; }
.task-group.later .task-row { color: var(--quiet); }
.tick { width: 24px; height: 24px; border-radius: 50%; border: 2px solid var(--quiet); box-sizing: border-box; background: none; flex: none; cursor: pointer; }
.task-group.later .tick { border-color: var(--line); }
.tick[aria-checked='true'] { background: var(--sage); border-color: var(--sage); }
.task-body { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1px; }
.task-title { font-size: 1rem; }
.task-sub { font-size: 0.78rem; color: var(--faint); }
.task-dur { font-size: 0.75rem; padding: 2px 9px; border-radius: 999px; border: 1px solid var(--line); color: var(--faint); font-variant-numeric: tabular-nums; }
.task-dur.est { border-style: dashed; }
.task-start { width: 36px; height: 36px; border: 0; border-radius: 50%; background: var(--sun); display: grid; place-items: center; cursor: pointer; flex: none; }
.task-start svg { width: 16px; height: 16px; fill: var(--ink); }
.task-steps { list-style: none; margin: 0; padding: 0 0 0 36px; }
.task-step { display: flex; align-items: center; gap: 12px; min-height: 40px; color: var(--quiet); font-size: 0.92rem; }
.task-step.done { color: var(--faint); text-decoration: line-through; text-decoration-color: var(--line); }
.task-step-min { margin-left: auto; font-size: 0.75rem; color: var(--faint); font-variant-numeric: tabular-nums; }
.task-done-fold { margin-top: 20px; display: flex; align-items: center; gap: 8px; font: inherit; font-size: 0.875rem; color: var(--faint); background: none; border: 0; padding: 0; cursor: pointer; }
.task-done-fold svg { width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.8; }
```

- [ ] **Step 3: Verify.** `pnpm build`; `pnpm shot tasks 390x844 /tmp/tasks.png` against `TasksMobile.dc.html`; `pnpm shot tasks 1440x900 /tmp/tasks-d.png` against `TasksDesktop.dc.html`. (The harness user has no tasks; add three through the UI once with the add field before shooting, or accept the empty list and check the add field's look.)

- [ ] **Step 4: Commit**

```bash
git add web/src/views/Tasks.tsx web/src/styles.css
git commit -m "feat: tasks in the horizon shape"
```

---

### Task 8: Chat, Memory, Settings

**Files:**
- Modify: `web/src/views/Talk.tsx`, `web/src/views/Memory.tsx`, `web/src/views/Settings.tsx`, `web/src/styles.css`

**Interfaces:**
- Consumes: `rememberConversation` (Task 3) — Talk calls it whenever it selects or creates a conversation so `TellNote` elsewhere continues the same thread; `readPrefs/writePrefs/prefsFrom`, `api.saveSettings` with the two new keys.

- [ ] **Step 1: Chat.** In `Talk.tsx`: composer placeholder `Tell Note`; call `rememberConversation(id)` where the active conversation id is set (after `api.talk` resolves and when the user opens a thread). The side list of conversations stays but opens from a small `Overflow`-style toggle at the top on mobile; no header title. Receipt rows: keep `.receipt` markup; the chip is a sage check SVG (`<svg viewBox="0 0 24 24"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>`), then the sentence, then a chevron. CSS: bubbles `.turn.user` = ink on `--ivory` text, radius 18px, max-width 300px (680px column on desktop, centred); `.turn.assistant` = `--haze-strong`; `.receipt` = 0.78rem `--faint`, the check `stroke: var(--sage)`; `.chat-composer` restyled as the `.tellnote` pill (add the `tellnote` class to the composer form and delete the old `.chat-composer/.chat-input/.chat-send` rules).

- [ ] **Step 2: Memory.** Search field: `.memory-search` becomes a `.tellnote`-styled pill with placeholder `Search what Note knows` and no button; remove `.memory-lede` and the category chips row (`.memory-chips`) from the markup. Rows: summary text, then the date (`created`, formatted `Aug 26` / `today`) right-aligned in `--faint`; drop `.memory-cat`. Detail (desktop pane / mobile after tap): title 1.625rem display, a meta line `From a chat on <date>` (the existing `.memory-meta` content, reworded to that shape), then two buttons: `That's wrong` (`.btn-haze`, calls `openTalk(\`This is wrong: "${fact.summary}". \`)`), `Tell Note more` (plain text button, calls `openTalk(\`About "${fact.summary}": \`)`). Remove `.memory-flag` / archived copy unless `archived` is true (then a single faint word `archived`).

- [ ] **Step 3: Settings.** Replace the six-section master-detail with one column of groups (mobile and desktop alike, `max-width: 560px`; desktop `padding-left: 120px`):

  - `HOME`: `Arc between sessions` (switch → `saveSettings({ show_arc_between_sessions })`, then `writePrefs(prefsFrom(saved))`); `Counter` (segmented `remaining | elapsed` → `saveSettings({ counter })`, then `writePrefs`).
  - `DAY`: `Routines and blocks · <n>` row that expands the existing `ScheduleList` inline; `Nightly letter` with the existing `<input type="time">`; `Time zone` with the existing datalist input. Each saves on change with the existing diff-against-baseline logic; show `✓ Saved` inline (`.pane-status.ok`).
  - `REACH`: `Push on this phone` switch (existing `PushSection` logic); `Calls` switch disabled with the sub-line `Needs a number`.
  - `NOTE`: `Your name` (display_name input), `How Note talks` (expands the existing `PersonaSection`), `Theme` (existing segmented control), and for admins `Server log` (expands `AdminSection`). `Sign out` as a plain text button at the end.

  Switch markup: `<button role="switch" aria-checked={on} className="sw" onClick=…/>`. CSS:

```css
/* ── horizon: settings ──────────────────────────────────────────── */
.settings { padding: 56px 24px 100px; max-width: 560px; display: flex; flex-direction: column; gap: 26px; }
@media (min-width: 768px) { .settings { padding: 46px 0 0 120px; } }
.set-group { display: flex; flex-direction: column; gap: 2px; }
.set-group-head { font-size: 0.72rem; letter-spacing: 0.12em; font-weight: 700; color: var(--faint); margin-bottom: 4px; }
.set-row { display: flex; align-items: center; gap: 10px; min-height: 56px; }
.set-row-body { flex: 1; display: flex; flex-direction: column; gap: 1px; }
.set-label { font-size: 1rem; color: var(--ink); }
.set-sub { font-size: 0.78rem; color: var(--faint); }
.set-value { font-size: 0.9rem; color: var(--quiet); font-variant-numeric: tabular-nums; }
.sw { width: 44px; height: 26px; border-radius: 13px; border: 0; background: var(--ink); position: relative; cursor: pointer; flex: none; padding: 0; }
.sw::after { content: ""; position: absolute; top: 3px; right: 3px; width: 20px; height: 20px; border-radius: 50%; background: var(--ivory); }
.sw[aria-checked='false'] { background: var(--line); }
.sw[aria-checked='false']::after { right: auto; left: 3px; }
.sw:disabled { opacity: 0.5; cursor: default; }
.sw:focus-visible { outline: 2px solid var(--ring); outline-offset: 2px; }
```

  Delete the `.set-side/.set-nav/.set-item/.set-pane/.card.pane/.pane-title` rules and the `<dialog>` confirm for prompt reset (replace with a toast + Undo that re-PUTs the previous content within 5 s, using the existing held-timer pattern).

- [ ] **Step 4: Verify.** `pnpm build`; shots of `chat`, `memory`, `settings` at 390x844 and 1440x900 against `ChatMobile/Desktop`, `MemoryMobile/Desktop`, `SettingsMobile/Desktop` boards. The settings shot must show the `Arc between sessions` switch on; toggling it off and reloading must show the between-sessions face as text (`pnpm shot today 390x844` after the toggle — do this by hand in a browser once, since the harness starts a fresh server each run).

- [ ] **Step 5: Commit**

```bash
git add web/src/views/Talk.tsx web/src/views/Memory.tsx web/src/views/Settings.tsx web/src/styles.css
git commit -m "feat: chat, memory and settings in the horizon shape"
```

---

### Task 9: Sweep

**Files:**
- Modify: `web/src/styles.css`, `web/src/receipts.ts` (if it still references removed views), `README.md` (the "Web client" section: the shell is now a top bar on desktop and tabs on mobile; Today lives under the home screen)

- [ ] **Step 1: Dead code.** `grep -n "spine\|nowcard\|sidebar\|mobile-head\|now-screen\|now-sheet\|--dawn\|--sunk\|--mist\|--moss\|--clay\|Fraunces" web/src/styles.css web/src/*.ts* web/src/views/*.tsx` — delete rules and imports that nothing uses; where an alias token is still used, leave the alias in `:root`. Delete `web/src/bell.tsx` and `web/src/navicon.tsx` only if unused (`navicon` is used by the tabs; keep it).
- [ ] **Step 2: Reduced motion.** Confirm every `animation:` in the sheet is covered by a `prefers-reduced-motion: reduce` override (`grep -n "animation:" web/src/styles.css`).
- [ ] **Step 3: Contrast.** `--faint` on `--earth` is ≥ 4.5:1 by the token values above; do not lighten it.
- [ ] **Step 4: README.** Update the "Web client" paragraph to describe Home (gauge in a session, wait arc between sessions, swipe up for Today), the desktop top bar, and the two new settings.
- [ ] **Step 5: Verify.** `pnpm build` clean; `cargo test` green; one last set of shots for all five tabs at both sizes; none reports an external request.
- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "chore: horizon sweep, docs and reduced-motion coverage"
```

---

## Self-review notes

- Spec → tasks: Global (Task 1, 5); Home in a session (Task 4); between sessions + setting (Tasks 2, 4, 8); Today (Tasks 3, 4, 6); Tasks (7); Chat (8); Memory (8); Settings (8); acceptance list (9).
- Names used consistently: `Gauge`, `DayLine`, `minutesOf`, `TellNote`, `rememberConversation`, `lastConversation`, `useStage`, `readPrefs/writePrefs/prefsFrom`, `api.setEventAlert`, `api.moveTomorrow`, `Home` props `{session,setSession,notify,onChanged,refresh,openNow,mobile,onChrome,tabs}`.
- `NowCounter` is reused as-is; its `mode` prop takes `CounterMode` from `session.ts`, which Task 2 keeps.
