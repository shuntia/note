# Daylight Step 9 — Vocabulary and Memory Surfaces Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace system vocabulary with user vocabulary across Memory, Settings and Talk, and give the Memory view its lede, dated rows, mockup-shaped detail pane, and an `Ask Note about this` hand-off into Talk.

**Architecture:** All changes are client-side under `web/src/`. The category identifiers `semantic`/`episodic`/`procedural` stay as API values and CSS class suffixes; only their rendered labels and tint assignments change. The list endpoint carries no timestamps, so a row's saved date comes from reading the fact itself — rows request their date as they scroll into view, through a module-level fact cache shared with the detail pane (which needs the same cache to resolve the superseded fact behind `Replaces a note from …`). Cross-view navigation is a single `openTalk(draft)` callback on the existing `ViewProps`, consumed by `Talk` on mount; no router.

**Tech Stack:** React 19, TypeScript 5.8, Vite 7, plain CSS with the existing token set. There is no frontend test runner in this repo — the verification cycle is `npx tsc --noEmit`, `npx vite build`, and a headless-browser pass against a stub API.

**Spec:** `docs/superpowers/plans/2026-09-01-daylight-ui-spec.md` (Step 9). Visual truth: `docs/superpowers/mockups/daylight/memory-desktop-light.html`, `settings-desktop-light.html`.

## Global Constraints

- Existing tokens only: `--sun` `#e8871e`, `--sun-ink`, `--moss`, `--clay`, the oklch monochrome scale in `web/src/styles.css`. No new fonts, no new global tokens (that is step 10). Never introduce red.
- Copy: sentence case, active voice, user vocabulary; no "semantic/episodic/procedural", no "IANA", no tool names in user-facing text. Replacement strings are reproduced character-for-character, em dashes included.
- Category tints: About you (`semantic`) = `--sun-ink`, Moments (`episodic`) = `--moss`, How you work (`procedural`) = `--clay`.
- Memory stays read-only. The empty-state sentence stays exactly as it is.
- Accessibility floor: visible keyboard focus on every interactive element, `prefers-reduced-motion` disables decorative animation, hit targets ≥ 40×40 px on touch layouts, text contrast ≥ 4.5:1.
- Write only under `web/` and this plan file. `server/` is read-only for this step; do not run `cargo`.

---

### Task 1: The four flat string replacements

**Files:**
- Modify: `web/src/views/Settings.tsx:220`, `:235`, `:241`, `:254`
- Modify: `web/src/views/Talk.tsx:492`
- Test: none (no frontend test runner) — `npx tsc --noEmit && npx vite build`, then a browser pass in Task 6.

**Interfaces:**
- Consumes: nothing.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Replace the timezone help string**

`web/src/views/Settings.tsx:220`

```tsx
<p className="pane-hint">Your days start and end here. Type a city to search.</p>
```

- [ ] **Step 2: Replace the nightly debrief help string**

`web/src/views/Settings.tsx:234-236`

```tsx
<p className="pane-hint">When Note writes the morning letter and plans tomorrow.</p>
```

- [ ] **Step 3: Replace the template label and help string**

`web/src/views/Settings.tsx:239-254` — the `<label>` text becomes `Shape of the day`, the hint becomes:

```tsx
<p className="pane-hint">Which routines and blocks make up a day.</p>
```

Leave `htmlFor="set-template"`, the `id`, and the `template` field name alone — those are wiring, not copy.

- [ ] **Step 4: Replace the composer placeholder**

`web/src/views/Talk.tsx:492`

```tsx
placeholder="Talk to Note — it can change the plan, tasks, and memory for you"
```

Nothing else in Talk changes in this step.

- [ ] **Step 5: Verify**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: both succeed.

- [ ] **Step 6: Commit** (fold into the Task 5 commit if executing straight through)

---

### Task 2: Memory vocabulary, tints, and the lede

**Files:**
- Modify: `web/src/views/Memory.tsx:16-24` (labels), `:100` (scope meta), `:105` (lede insertion point)
- Modify: `web/src/styles.css:1227-1229` (tint map), new `.memory-lede` rule

**Interfaces:**
- Consumes: nothing.
- Produces: `LABEL` keeps its `Record<Category, string>` shape; `categoryLabel(category: string): string` keeps its signature.

- [ ] **Step 1: Relabel the categories**

`web/src/views/Memory.tsx` — the `CATEGORIES` tuple and every API call keep the identifiers:

```tsx
const LABEL: Record<Category, string> = {
  semantic: 'About you',
  episodic: 'Moments',
  procedural: 'How you work',
}
```

- [ ] **Step 2: Stop rendering the raw identifier in the count meta**

`web/src/views/Memory.tsx:100` currently reads `const scope = searching ? 'found' : filter === 'all' ? 'saved' : filter`, which renders `12 semantic`. Replace with:

```tsx
const scope = searching ? 'found' : 'saved'
```

- [ ] **Step 3: Add the lede under the title**

Immediately after the `<SectionTitle …>Memory</SectionTitle>` line:

```tsx
<p className="memory-lede">
  What Note has learned as you talk. Nothing here is ever deleted — replaced notes move to the
  archive.
</p>
```

The em dash is U+2014 and the sentence is reproduced exactly.

- [ ] **Step 4: Swap the two tints and style the lede**

`web/src/styles.css` — replace the three `.cat-*` rules with:

```css
.cat-semantic { --cat: var(--sun-ink); }
.cat-episodic { --cat: var(--moss); }
.cat-procedural { --cat: var(--clay); }
```

and add, next to `.memory-side`:

```css
.memory-lede {
  margin: -0.2rem 0 0.15rem;
  max-width: 46ch;
  font-size: 0.85rem;
  line-height: 1.45;
  color: var(--text-muted);
}
```

- [ ] **Step 5: Verify**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: both succeed.

---

### Task 3: Dated list rows

**Files:**
- Modify: `web/src/views/Memory.tsx` (fact cache, `relativeDay`, `savedLabel`, `MemoryRow`)
- Modify: `web/src/styles.css` (`.memory-rowmeta`)

**Interfaces:**
- Produces, for Task 4:
  - `const factCache = new Map<string, MemoryFact>()` — module scope, survives view remounts; facts are immutable once written, so entries never expire.
  - `function relativeDay(iso: string): string` — `'today' | 'yesterday' | '<n> days ago' | 'last week' | 'Aug 20' | 'Aug 20, 2025'`; `''` for an unparseable date.
  - `function shortDate(iso: string): string` — unchanged signature, month + day, plus year when it differs from now.
  - `useFacts()` returning `{ get: (id: string) => MemoryFact | undefined; want: (id: string) => void }`.

- [ ] **Step 1: Add the date helpers**

```tsx
const DAY_MS = 86_400_000

// Day-boundary distance, not elapsed hours: 23:00 yesterday reads "yesterday".
function relativeDay(iso: string): string {
  const at = new Date(iso)
  if (Number.isNaN(at.getTime())) return ''
  const start = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()
  const days = Math.round((start(new Date()) - start(at)) / DAY_MS)
  if (days <= 0) return 'today'
  if (days === 1) return 'yesterday'
  if (days < 7) return `${days} days ago`
  if (days < 14) return 'last week'
  return shortDate(iso)
}

function savedLabel(iso: string): string {
  const day = relativeDay(iso)
  return day === 'today' || day === 'yesterday' ? `saved ${day}` : day
}
```

`shortDate` becomes month + day, with the year only when it is not the current one:

```tsx
function shortDate(iso: string): string {
  const at = new Date(iso)
  if (Number.isNaN(at.getTime())) return ''
  const sameYear = at.getFullYear() === new Date().getFullYear()
  return at.toLocaleDateString(undefined, {
    month: 'short',
    day: 'numeric',
    ...(sameYear ? {} : { year: 'numeric' }),
  })
}
```

- [ ] **Step 2: Add the fact cache hook**

```tsx
const factCache = new Map<string, MemoryFact>()
const inFlight = new Set<string>()

// The list endpoint carries summaries only, so a row's date and a supersede
// target both come from reading the fact itself.
function useFacts() {
  const [, bump] = useState(0)
  const want = useCallback((id: string) => {
    if (factCache.has(id) || inFlight.has(id)) return
    inFlight.add(id)
    api
      .memoryRead(id)
      .then((f) => {
        factCache.set(id, f)
        bump((n) => n + 1)
      })
      .catch(() => {
        // a date that will not load simply stays off the row
      })
      .finally(() => inFlight.delete(id))
  }, [])
  return { get: (id: string) => factCache.get(id), want }
}
```

- [ ] **Step 3: Move the row into its own component that asks for its date when seen**

```tsx
function MemoryRow({
  hit,
  saved,
  selected,
  onOpen,
  onSeen,
}: {
  hit: MemoryHit
  saved: string | undefined
  selected: boolean
  onOpen: (id: string) => void
  onSeen: (id: string) => void
}) {
  const row = useRef<HTMLLIElement>(null)
  useEffect(() => {
    if (saved !== undefined) return
    const el = row.current
    if (!el || typeof IntersectionObserver !== 'function') {
      onSeen(hit.id)
      return
    }
    const io = new IntersectionObserver(
      (entries) => {
        if (!entries.some((e) => e.isIntersecting)) return
        io.disconnect()
        onSeen(hit.id)
      },
      { rootMargin: '200px' },
    )
    io.observe(el)
    return () => io.disconnect()
  }, [hit.id, saved, onSeen])

  return (
    <li ref={row}>
      <button className="memory-row" aria-current={selected} onClick={() => onOpen(hit.id)}>
        <span className="memory-summary">{hit.summary}</span>
        <span className="memory-rowmeta">
          <CategoryChip category={hit.category} />
          {saved && <span>{savedLabel(saved)}</span>}
        </span>
      </button>
    </li>
  )
}
```

- [ ] **Step 4: Render rows through it**

In `Memory`, call `const facts = useFacts()` and replace the `items.map(...)` body with:

```tsx
<MemoryRow
  key={m.id}
  hit={m}
  saved={facts.get(m.id)?.created}
  selected={selected === m.id}
  onOpen={openFact}
  onSeen={facts.want}
/>
```

`openFact` also seeds the cache on success (`factCache.set(f.id, f)`) so opening a fact dates its row for free.

- [ ] **Step 5: Style the row meta line**

```css
.memory-rowmeta {
  display: flex;
  align-items: center;
  gap: 0.45rem;
  font-size: 0.72rem;
  color: var(--text-muted);
}
```

- [ ] **Step 6: Verify**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: both succeed.

---

### Task 4: Detail pane per the mockup

**Files:**
- Modify: `web/src/views/Memory.tsx` (`FactBody`)
- Modify: `web/src/styles.css` (`.memory-dmeta`, `.memory-ask`, `.memory-meta` reshape)

**Interfaces:**
- Consumes: `factCache`, `useFacts`, `relativeDay`, `shortDate` from Task 3.
- Produces: `FactBody` gains props `{ fact: MemoryFact; facts: ReturnType<typeof useFacts>; onOpen: (id: string) => void; onAsk: (fact: MemoryFact) => void }`.

- [ ] **Step 1: Reorder the detail to chip → title → body → rule → meta**

```tsx
function FactBody({ fact, facts, onOpen, onAsk }) {
  const previous = fact.supersedes
  useEffect(() => {
    if (previous) facts.want(previous)
  }, [previous, facts])
  const older = previous ? facts.get(previous) : undefined
  const at = new Date(fact.created)
  const time = Number.isNaN(at.getTime())
    ? ''
    : at.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })

  return (
    <article>
      <p className="memory-meta">
        <CategoryChip category={fact.category} />
        {fact.archived && <span className="memory-flag">Archived</span>}
      </p>
      <h2 className="memory-title">{fact.summary}</h2>
      <Markdown text={fact.body} />
      <div className="memory-dmeta">
        <span>
          Saved from Talk · {relativeDay(fact.created)}
          {time && `, ${time}`}
        </span>
        {older && (
          <span>
            Replaces a note from {shortDate(older.created)} ·{' '}
            <button className="memory-prev" onClick={() => onOpen(older.id)}>
              see what changed
            </button>
          </span>
        )}
      </div>
      <button className="memory-ask" onClick={() => onAsk(fact)}>
        Ask Note about this
      </button>
    </article>
  )
}
```

The `Replaces a note from …` line renders only once the superseded fact has actually been read back, so a link that would not resolve never appears.

- [ ] **Step 2: Restyle the meta rows**

`.memory-meta` loses its bottom rule and becomes the chip row above the title; the rule moves to `.memory-dmeta`:

```css
.memory-meta {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 0.55rem;
  margin: 0 0 0.5rem;
  font-size: 0.78rem;
  color: var(--text-muted);
}
.memory-dmeta {
  display: grid;
  gap: 0.3rem;
  margin-top: 1.15rem;
  padding-top: 0.9rem;
  border-top: 1px solid var(--border);
  font-size: 0.78rem;
  color: var(--text-muted);
}
.memory-prev { color: var(--sun-ink); }
.memory-ask {
  display: inline-flex;
  align-items: center;
  min-height: 2.5rem;
  margin-top: 1rem;
  padding: 0.4rem 0.95rem;
  border: 1px solid color-mix(in srgb, var(--accent) 35%, var(--border));
  border-radius: 999px;
  background: none;
  color: var(--sun-ink);
  font: inherit;
  font-size: 0.85rem;
  cursor: pointer;
  transition: background 120ms ease;
}
.memory-ask:hover { background: color-mix(in srgb, var(--accent) 10%, transparent); }
```

Add `.memory-ask` to the `prefers-reduced-motion` block that already neutralises `.chip, .memory-row` transitions. `min-height: 2.5rem` holds the 40 px touch floor.

- [ ] **Step 3: Verify**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: both succeed.

---

### Task 5: `Ask Note about this` lands in Talk

**Files:**
- Modify: `web/src/app.tsx` (`ViewProps`, shell state, `Talk` props)
- Modify: `web/src/views/Talk.tsx` (accept and consume the prefill)
- Modify: `web/src/views/Memory.tsx` (wire `onAsk`)

**Interfaces:**
- Produces: `ViewProps` gains `openTalk: (draft: string) => void`. `Talk` gains an optional prop `prefill?: string` and `onPrefilled?: () => void`.

- [ ] **Step 1: Add the navigation payload to the shell**

`web/src/app.tsx`:

```tsx
export type ViewProps = {
  notify: (msg: string, action?: ToastAction) => void
  refresh: number
  onChanged: () => void
  // Switches to Talk with a new conversation and this text waiting in the composer.
  openTalk: (draft: string) => void
}
```

In `App`:

```tsx
const [talkPrefill, setTalkPrefill] = useState<string | null>(null)
const openTalk = useCallback((draft: string) => {
  setTalkPrefill(draft)
  setTab('chat')
}, [])
```

`const views: ViewProps = { notify, refresh, onChanged, openTalk }`, and the Talk line becomes:

```tsx
{tab === 'chat' && (
  <Talk {...views} prefill={talkPrefill} onPrefilled={() => setTalkPrefill(null)} />
)}
```

- [ ] **Step 2: Consume the prefill on mount in Talk**

`Talk` takes `{ notify, prefill, onPrefilled }` and, after the existing effects:

```tsx
useEffect(() => {
  if (!prefill) return
  setDraft(prefill)
  const el = input.current
  el?.focus()
  el?.setSelectionRange(prefill.length, prefill.length)
  onPrefilled?.()
}, [prefill, onPrefilled])
```

Talk unmounts on every view switch, so it always opens on a new conversation (`current` starts `null`) — nothing extra is needed to satisfy "a new conversation". Nothing is sent: `send()` only runs from Enter or the send button.

- [ ] **Step 3: Wire the Memory button**

`Memory` destructures `openTalk` from its props and passes:

```tsx
onAsk={(f) => openTalk(`About the memory "${f.summary}" — `)}
```

The trailing space after the em dash is part of the string.

- [ ] **Step 4: Verify**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: both succeed.

- [ ] **Step 5: Commit**

```bash
git add web/src/app.tsx web/src/views/Memory.tsx web/src/views/Settings.tsx \
  web/src/views/Talk.tsx web/src/styles.css \
  docs/superpowers/plans/2026-09-01-daylight-step-9-vocabulary.md
git commit -m "feat: user vocabulary and memory detail surfaces"
```

Stage the listed paths explicitly — another agent is editing `server/` concurrently.

---

### Task 6: Browser verification against a stub API

**Files:**
- Create (scratchpad only, never committed): a stub server serving `web/dist` plus `/api/*` fixtures.

- [ ] **Step 1: Serve fixtures**

Fixtures must include: three facts across all three categories; one fact whose `supersedes` points at an archived fact that the stub's `/api/memory/:id` also serves; one fact with no `supersedes`.

- [ ] **Step 2: Walk the acceptance list**

- Filter chips read `All`, `About you`, `Moments`, `How you work`.
- The lede renders under the title.
- Rows show summary, tinted chip, and a saved date.
- Detail shows chip, serif title, body, rule, `Saved from Talk · …`.
- The supersede line appears on the superseding fact only, and clicking `see what changed` opens the archived fact.
- `Ask Note about this` switches to Talk with `About the memory "<summary>" — ` in a focused composer, unsent, on a new chat.
- Settings shows the three replaced strings; the composer placeholder is the new one.

- [ ] **Step 3: Grep the built bundle**

Run: `grep -o "semantic\|episodic\|procedural\|IANA" web/dist/assets/*.js web/dist/assets/*.css | sort | uniq -c`
Expected: only bare category identifiers (API values and `cat-*` class suffixes) survive; no `Semantic`/`Episodic`/`Procedural` label strings and no `IANA` anywhere.
