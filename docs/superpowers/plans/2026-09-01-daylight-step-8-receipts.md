# Daylight Step 8 — Tool-call receipts (Talk) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every agent tool call in Talk renders as a one-line human receipt chip that expands to the raw call, and conversation rename/delete move behind a `⋯` menu with a 10 s undoable delete.

**Architecture:** A pure, dependency-free module (`web/src/receipts.ts`) turns `(tool name, args JSON, is_error)` into a sentence; `Talk.tsx` renders consecutive tool items as a grouped column of `<Receipt>` chips instead of the raw `<details>` block. The `⋯` menu from Today's event card is factored into `web/src/overflow.tsx` and reused by both call sites. Delete follows the repo's existing held-request pattern (module-scope timer, request fired only when the undo window closes), with the shell's toast extended to carry a per-toast window length.

**Tech Stack:** React 19 + TypeScript + Vite. No test runner exists in `web/`; verification is `npx tsc --noEmit`, `npx vite build`, and a headless-browser pass against a stub API.

**Spec:** `docs/superpowers/plans/2026-09-01-daylight-ui-spec.md` — step 8, plus the Global constraints section.

## Global Constraints

- Design tokens: existing `--sun` `#e8871e` accent and the oklch monochrome scale in `web/src/styles.css` only. New tokens are step 10. Accent = amber only; `--moss` = done; `--clay` = dropped/warn. **Never introduce red** — a failed receipt is clay, not `--danger`.
- Copy: sentence case; active voice; past tense for receipts; an action keeps its name through its whole flow; user vocabulary, never system vocabulary. The raw tool name appears **only** inside the expanded card — never in the chip sentence, except the `✓ Used <tool name>` unknown-tool fallback the spec allows.
- Every state-changing action gets feedback within 100 ms (optimistic) and, where destructive, an in-place Undo. Nothing confirm-dialog-guarded that can be undo-guarded.
- Accessibility floor: visible keyboard focus on all interactive elements; `prefers-reduced-motion` disables decorative animation; hit targets ≥ 40×40 px on touch layouts; text contrast ≥ 4.5:1.
- All assets bundled; the client makes no external requests.
- Out of scope, owned by later agents: the `Message Note…` → `Talk to Note — …` placeholder change (step 9) and any chat restyling beyond the assistant avatar + measured column named in 8.5.

---

## File Structure

| File | Responsibility |
|---|---|
| `web/src/receipts.ts` (create) | Per-tool sentence templates + the shared event-kind humanizer. Pure functions, no React. |
| `web/src/overflow.tsx` (create) | The `⋯` menu extracted from `Today.tsx`, generalized to N items. |
| `web/src/views/Talk.tsx` (modify) | Receipt chips replace `ToolBlock`; conversation row uses the shared `⋯`; held 10 s delete; assistant avatar; standing line under composer. |
| `web/src/views/Today.tsx` (modify) | Drops its local `Overflow` and `label` in favour of the shared ones. |
| `web/src/app.tsx` (modify) | `ToastAction` gains `windowMs`; `Talk` receives `ViewProps`. |
| `web/src/styles.css` (modify) | `.receipt*`, `.turn-avatar`, `.chat-standing`, `.chat-more*`; removes the now-dead `.tool*` and `.chat-tool*` rules. |

## Tool inventory (enumerated from `server/src/tools/`)

`registry()` in `server/src/tools/mod.rs` defines three nested surfaces; the union is 11 tools. Talk exposes 9 of them, but a conversation can also replay messages written by nightly/check-in sessions, so all 11 need templates.

| Tool | Args (from the `*Args` structs) | Session surfaces |
|---|---|---|
| `task_create` | `title`, `description?` | checkin, talk, nightly |
| `task_update` | `task_id`, `title?`, `description?`, `state?`, `notes?` | checkin, talk, nightly |
| `memory_query` | `query`, `limit?` | checkin, talk, nightly |
| `memory_read` | `id` | checkin, talk, nightly |
| `memory_write` | `op` (`add`/`update`/`supersede`), `id?`, `category?`, `summary`, `body` | checkin, talk, nightly |
| `context_edit` | `find?`, `replace?`, `append?` | talk, nightly |
| `schedule_slide` | `event_id`, `minutes` (negative = earlier) | checkin, talk, nightly |
| `schedule_snooze` | `event_id`, `minutes` (1..=1440) | checkin, talk, nightly |
| `schedule_drop` | `event_id` | checkin, talk, nightly |
| `schedule_insert` | `date`, `kind`, `time`, `flexibility`, `slide_window_min?`, `channel` | nightly |
| `notify_send` | `text` | nightly |

Only `schedule_insert` and `notify_send` carry a human-readable name/text in their args; the schedule ops that take a bare `event_id` cannot name the event, so their sentences say "an event" rather than inventing a title.

---

### Task 1: Receipt sentence templates

**Files:**
- Create: `web/src/receipts.ts`
- Modify: `web/src/views/Today.tsx` (import `eventLabel`, delete local `label`)

**Interfaces:**
- Produces: `eventLabel(kind: string): string`; `receipt(name: string, args: string, isError: boolean): string`.

- [ ] **Step 1: Write `web/src/receipts.ts`**

The module exports one entry point. `args` arrives as a raw JSON string that may be empty or malformed; parsing failures degrade to the tool's generic sentence rather than throwing.

```ts
type Args = Record<string, unknown>

const MAX_QUOTE = 80

const str = (a: Args, key: string): string => (typeof a[key] === 'string' ? (a[key] as string).trim() : '')
const num = (a: Args, key: string): number | null =>
  typeof a[key] === 'number' && Number.isFinite(a[key]) ? (a[key] as number) : null

function clip(text: string): string {
  const flat = text.replace(/\s+/g, ' ').trim()
  return flat.length > MAX_QUOTE ? `${flat.slice(0, MAX_QUOTE - 1)}…` : flat
}

const quoted = (text: string) => `“${clip(text)}”`

function span(minutes: number): string {
  const m = Math.abs(minutes)
  if (m < 60 || m % 60 !== 0) return `${m} min`
  const hours = m / 60
  return `${hours} hr`
}

export function eventLabel(kind: string): string {
  if (kind === 'debrief') return 'Morning debrief'
  const words = kind.replaceAll('_', ' ').replace('checkin', 'check-in').trim()
  return words.charAt(0).toUpperCase() + words.slice(1)
}

function dayWord(iso: string): string {
  const at = new Date(`${iso}T00:00`)
  if (Number.isNaN(at.getTime())) return iso
  const days = Math.round((at.setHours(0, 0, 0, 0) - new Date().setHours(0, 0, 0, 0)) / 86_400_000)
  if (days === 0) return 'today'
  if (days === 1) return 'tomorrow'
  return new Date(`${iso}T00:00`).toLocaleDateString(undefined, { weekday: 'short', month: 'short', day: 'numeric' })
}
```

Then the two maps. `done` sentences are past tense; `failed` sentences all start `Couldn't `.

```ts
const done: Record<string, (a: Args) => string> = {
  task_create: (a) => {
    const title = str(a, 'title')
    return title ? `Added ${quoted(title)} to your tasks` : 'Added a task'
  },
  task_update: (a) => {
    const title = str(a, 'title')
    switch (str(a, 'state')) {
      case 'done':
        return 'Marked a task done'
      case 'dropped':
        return 'Dropped a task'
      case 'in_progress':
        return 'Started a task'
      case 'open':
        return 'Reopened a task'
    }
    if (title) return `Renamed a task to ${quoted(title)}`
    if (str(a, 'notes')) return 'Added a note to a task'
    if (str(a, 'description')) return "Filled in a task's details"
    return 'Updated a task'
  },
  memory_query: (a) => {
    const q = str(a, 'query')
    return q ? `Searched memory for ${quoted(q)}` : 'Searched memory'
  },
  memory_read: () => 'Read a saved note',
  memory_write: (a) => {
    const summary = str(a, 'summary')
    const tail = summary ? `: ${clip(summary)}` : ''
    if (str(a, 'op') === 'update') return `Updated a saved note${tail}`
    if (str(a, 'op') === 'supersede') return `Replaced an older note${tail}`
    return `Remembered${tail}`
  },
  context_edit: (a) => {
    const append = str(a, 'append')
    return append ? `Added ${quoted(append)} to your background notes` : 'Updated your background notes'
  },
  schedule_slide: (a) => {
    const minutes = num(a, 'minutes')
    if (minutes === null || minutes === 0) return 'Moved an event'
    return `Moved an event ${span(minutes)} ${minutes < 0 ? 'earlier' : 'later'}`
  },
  schedule_snooze: (a) => {
    const minutes = num(a, 'minutes')
    return minutes === null ? 'Put an event off' : `Put an event off for ${span(minutes)}`
  },
  schedule_drop: () => 'Dropped an event from the day',
  schedule_insert: (a) => {
    const kind = str(a, 'kind')
    const name = kind ? quoted(eventLabel(kind)) : 'an event'
    const date = str(a, 'date')
    const time = str(a, 'time')
    const when = [date ? dayWord(date) : '', time ? `at ${time}` : ''].filter(Boolean).join(' ')
    return when ? `Added ${name} to ${when}` : `Added ${name} to the plan`
  },
  notify_send: (a) => {
    const text = str(a, 'text')
    return text ? `Sent you a nudge: ${clip(text)}` : 'Sent you a nudge'
  },
}

const failed: Record<string, string> = {
  task_create: "Couldn't add that task",
  task_update: "Couldn't update that task",
  memory_query: "Couldn't search memory",
  memory_read: "Couldn't open that note",
  memory_write: "Couldn't save that to memory",
  context_edit: "Couldn't update your background notes",
  schedule_slide: "Couldn't move that event",
  schedule_snooze: "Couldn't put that event off",
  schedule_drop: "Couldn't drop that event",
  schedule_insert: "Couldn't add that to the plan",
  notify_send: "Couldn't send that nudge",
}

function parse(raw: string): Args {
  try {
    const value: unknown = JSON.parse(raw)
    return typeof value === 'object' && value !== null ? (value as Args) : {}
  } catch {
    return {}
  }
}

/// The chip sentence for one call; unknown tools still get a chip, never a raw block.
export function receipt(name: string, args: string, isError: boolean): string {
  if (isError) return failed[name] ?? `Couldn't use ${name}`
  const template = done[name]
  return template ? template(parse(args)) : `Used ${name}`
}
```

- [ ] **Step 2: Point `Today.tsx` at the shared humanizer**

Delete the local `label` function in `web/src/views/Today.tsx` and replace its three call sites (`EventRow`, `NowCard`, `drop`) with `eventLabel`, importing `import { eventLabel } from '../receipts'`.

- [ ] **Step 3: Typecheck**

Run: `cd web && npx tsc --noEmit`
Expected: no output (Today still compiles with the shared label; `receipts.ts` has no unused exports errors since `noUnusedLocals` applies per-file).

- [ ] **Step 4: Commit**

```bash
git add web/src/receipts.ts web/src/views/Today.tsx
git commit -m "feat: per-tool receipt sentences"
```

---

### Task 2: Shared overflow menu

**Files:**
- Create: `web/src/overflow.tsx`
- Modify: `web/src/views/Today.tsx` (delete local `Overflow`, call the shared one)
- Modify: `web/src/styles.css` (menu rules keyed off a shared class)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: `type OverflowItem = { label: string; run: () => void; disabled?: boolean }` and `function Overflow({ label, items, className }: { label: string; items: OverflowItem[]; className?: string }): JSX.Element`.

- [ ] **Step 1: Write `web/src/overflow.tsx`**

Lifted verbatim from Today's existing menu, with two changes: N items instead of one, and a focus-out test that does not close the menu while focus moves between its own items.

```tsx
import { useEffect, useRef, useState, type FocusEvent } from 'react'

export type OverflowItem = { label: string; run: () => void; disabled?: boolean }

export function Overflow({
  label,
  items,
  className = 'ev-more-wrap',
}: {
  label: string
  items: OverflowItem[]
  className?: string
}) {
  const [open, setOpen] = useState(false)
  const wrap = useRef<HTMLDivElement>(null)
  const trigger = useRef<HTMLButtonElement>(null)
  const first = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    if (!open) return
    first.current?.focus()
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

  const onBlur = (e: FocusEvent<HTMLDivElement>) => {
    if (!wrap.current?.contains(e.relatedTarget as Node | null)) setOpen(false)
  }

  return (
    <div className={className} ref={wrap}>
      <button
        className="ev-more"
        ref={trigger}
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        ⋯
      </button>
      {open && (
        <div className="ev-menu" role="menu" onBlur={onBlur}>
          {items.map((item, i) => (
            <button
              key={item.label}
              className="ev-menu-item"
              role="menuitem"
              ref={i === 0 ? first : undefined}
              disabled={item.disabled}
              onClick={() => {
                setOpen(false)
                item.run()
              }}
            >
              {item.label}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}
```

- [ ] **Step 2: Replace Today's local menu**

In `web/src/views/Today.tsx`: delete the whole `function Overflow(...)` block and the now-unused `useRef`/`useState` imports only if nothing else needs them (they are still needed — `Today` uses both). Change the `NowCard` call site to:

```tsx
<Overflow
  label="More actions"
  items={[{ label: 'Drop', run: () => drop(ev), disabled: pending }]}
/>
```

and add `import { Overflow } from '../overflow'`.

- [ ] **Step 3: Typecheck and build**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: both succeed.

- [ ] **Step 4: Commit**

```bash
git add web/src/overflow.tsx web/src/views/Today.tsx
git commit -m "refactor: shared overflow menu"
```

---

### Task 3: Per-toast undo window

**Files:**
- Modify: `web/src/app.tsx:20` (`ToastAction`) and `web/src/app.tsx:46-50` (`notify`), plus the `Talk` render site.

**Interfaces:**
- Produces: `ToastAction = { label: string; run: () => void; windowMs?: number }`; `<Talk {...views} />` so Talk receives `ViewProps`.

- [ ] **Step 1: Widen `ToastAction`**

```ts
// `windowMs` is the undo window the action holds open; the toast must outlast it.
export type ToastAction = { label: string; run: () => void; windowMs?: number }
```

- [ ] **Step 2: Honour it in `notify`**

```ts
toastTimer.current = window.setTimeout(
  () => setToast(null),
  action ? (action.windowMs ?? 5000) : 4000,
)
```

- [ ] **Step 3: Hand Talk the shell props**

`{tab === 'chat' && <Talk {...views} />}` — no `key`, so the pane is not remounted by unrelated refreshes.

- [ ] **Step 4: Typecheck**

Run: `cd web && npx tsc --noEmit`
Expected: fails with `Property 'notify' … not assignable` only if Talk's signature has not been widened yet — that lands in Task 4. Run the typecheck at the end of Task 4 instead if executing Tasks 3 and 4 together; otherwise widen `Talk`'s signature to `({ notify }: ViewProps)` in this task and leave `notify` unused until Task 4 wires it.

- [ ] **Step 5: Commit (with Task 4)**

The shell change is meaningless on its own; commit it with the Talk work.

---

### Task 4: Receipts, avatar, standing line, and the ⋯ conversation menu in Talk

**Files:**
- Modify: `web/src/views/Talk.tsx`
- Modify: `web/src/styles.css`

**Interfaces:**
- Consumes: `receipt` from Task 1, `Overflow`/`OverflowItem` from Task 2, `ToastAction.windowMs` and `ViewProps` from Task 3.

- [ ] **Step 1: Replace `ToolBlock` with `Receipt`**

Delete `function ToolBlock`. Add:

```tsx
const UNDO_MS = 10_000

function Receipt({ item }: { item: ToolItem }) {
  const [open, setOpen] = useState(false)
  const args = pretty(item.args)
  const result = pretty(item.result)
  return (
    <div className={item.isError ? 'receipt error' : 'receipt'}>
      <button className="receipt-chip" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
        <span className="receipt-mark" aria-hidden="true">
          {item.isError ? '✕' : '✓'}
        </span>
        <span className="receipt-text">{receipt(item.name, item.args, item.isError)}</span>
        <span className="receipt-chev" aria-hidden="true">
          {open ? '▾' : '▸'}
        </span>
      </button>
      {open && (
        <div className="receipt-body">
          <div className="receipt-tool">{item.name}</div>
          {args && <pre className="receipt-block">{args}</pre>}
          <pre className="receipt-block">{result || '—'}</pre>
        </div>
      )}
    </div>
  )
}
```

- [ ] **Step 2: Group consecutive tool items**

The mockup stacks receipts as one tight column **under** the reply, while the transcript records the calls before it and the stream's own gap is 1.35 rem. Fold runs of tool items into a single node, then swap each tool group with the assistant group that follows it so the chips sit under the sentence that explains them:

```tsx
type Group = { key: string; items: Item[] }

function grouped(items: Item[]): Group[] {
  const out: Group[] = []
  for (const item of items) {
    const last = out[out.length - 1]
    if (item.kind === 'tool' && last?.items[0]?.kind === 'tool') last.items.push(item)
    else out.push({ key: item.key, items: [item] })
  }
  for (let i = 0; i < out.length - 1; i++) {
    if (out[i].items[0].kind === 'tool' && out[i + 1].items[0].kind === 'assistant') {
      ;[out[i], out[i + 1]] = [out[i + 1], out[i]]
      i++
    }
  }
  return out
}
```

and render `grouped(items).map((g) => g.items[0].kind === 'tool' ? (<div key={g.key} className="receipts">{g.items.map((i) => <Receipt key={i.key} item={i as ToolItem} />)}</div>) : renderOne(g.items[0]))`, where `renderOne` is the existing per-item switch.

- [ ] **Step 3: Assistant avatar and measured column**

```tsx
<div key={item.key} className="turn assistant">
  <span className="turn-avatar" aria-hidden="true" />
  <Markdown text={item.text} />
</div>
```

- [ ] **Step 4: Standing line under the composer**

Inside `.chat-foot`, after the `<form>`:

```tsx
<p className="chat-standing">Every change Note makes shows up above — nothing happens silently.</p>
```

- [ ] **Step 5: Conversation row ⋯ menu + held delete**

Delete the `confirming` state and the inline `rename`/`delete` buttons. Above the component:

```ts
// The API has no undelete, so the request waits out the undo window before it is sent.
let heldDelete: { id: number; timer: number } | null = null
```

Inside the component:

```tsx
const commitDelete = useCallback(() => {
  if (!heldDelete) return
  const { id, timer } = heldDelete
  heldDelete = null
  window.clearTimeout(timer)
  api.deleteConversation(id).then(
    () => void loadList(true),
    () => void loadList(true),
  )
}, [loadList])

useEffect(() => commitDelete, [commitDelete])

const remove = (c: Conversation) => {
  commitDelete()
  heldDelete = { id: c.id, timer: window.setTimeout(commitDelete, UNDO_MS) }
  const wasOpen = current === c.id
  if (wasOpen) {
    era.current++
    wanted.current = null
    setCurrent(null)
    setItems([])
    setMsgState('ready')
  }
  setSideNotice(null)
  tick((n) => n + 1)
  notify(`Deleted "${c.title}"`, {
    label: 'Undo',
    windowMs: UNDO_MS,
    run: () => {
      if (heldDelete?.id !== c.id) return
      window.clearTimeout(heldDelete.timer)
      heldDelete = null
      tick((n) => n + 1)
      if (wasOpen) open(c.id)
    },
  })
}
```

with `const [, tick] = useState(0)` alongside the other state, and the list rendered from `conversations.filter((c) => c.id !== heldDelete?.id)`.

The row's meta becomes:

```tsx
<div className="chat-meta">
  <span className="chat-when">{shortDate(c.updated_at)}</span>
  <Overflow
    className="chat-more-wrap"
    label={`More actions for ${c.title}`}
    items={[
      { label: 'Rename', run: () => setRenaming({ id: c.id, value: c.title }) },
      { label: 'Delete', run: () => remove(c) },
    ]}
  />
</div>
```

The spec writes this toast as `Deleted "<title>" — Undo`, where the trailing word names the button — the same shape as step 3's `Saved to Tasks — Undo`, which shipped as the message `Saved to Tasks` plus an `Undo` button. Follow that precedent so the word appears once.

- [ ] **Step 6: Styles**

Add to `web/src/styles.css`, using existing tokens only:

```css
/* tool-call receipts — a sentence first; the raw call is one chevron away */
.receipts { display: flex; flex-direction: column; gap: 0.4rem; margin-left: 2.375rem; }
.receipt { max-width: 33rem; }
.receipt-chip {
  display: flex;
  align-items: center;
  gap: 0.55rem;
  width: fit-content;
  max-width: 100%;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 999px;
  padding: 0.3rem 0.85rem 0.3rem 0.7rem;
  color: var(--text-muted);
  font: inherit;
  font-size: 0.85rem;
  text-align: left;
  cursor: pointer;
}
.receipt-chip:hover { color: var(--text); border-color: var(--border-input); }
.receipt-chip:focus-visible { outline: 2px solid var(--ring); outline-offset: 2px; }
.receipt.open .receipt-chip, .receipt:has(.receipt-body) .receipt-chip {
  border-radius: var(--radius-lg) var(--radius-lg) 0 0;
  width: 100%;
}
.receipt-mark { flex: none; color: var(--moss); font-size: 0.9rem; line-height: 1; }
.receipt.error .receipt-mark { color: var(--clay); }
.receipt-text { min-width: 0; overflow-wrap: anywhere; }
.receipt-chev { flex: none; margin-left: auto; font-size: 0.7rem; color: var(--text-muted); }
.receipt-body {
  border: 1px solid var(--border);
  border-top: none;
  border-radius: 0 0 var(--radius-lg) var(--radius-lg);
  background: var(--surface);
  padding: 0.6rem 0.75rem 0.7rem;
  display: grid;
  gap: 0.35rem;
}
.receipt.error .receipt-chip, .receipt.error .receipt-body {
  border-color: color-mix(in srgb, var(--clay) 40%, var(--border));
}
.receipt-tool {
  font-family: var(--mono);
  font-size: 0.68rem;
  letter-spacing: 0.06em;
  color: var(--text-muted);
}
.receipt-block {
  margin: 0;
  background: var(--code-bg);
  border-radius: var(--radius);
  padding: 0.5rem 0.65rem;
  font-family: var(--mono);
  font-size: 0.75rem;
  line-height: 1.5;
  max-height: 14rem;
  overflow: auto;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.turn.assistant { display: flex; align-items: flex-start; gap: 0.75rem; }
.turn-avatar {
  width: 26px;
  height: 26px;
  flex: none;
  margin-top: 0.25rem;
  border-radius: 50%;
  background: radial-gradient(circle at 35% 35%, color-mix(in srgb, var(--sun) 55%, white), var(--sun));
}
.turn.assistant .prose { min-width: 0; max-width: 35rem; }

.chat-standing {
  max-width: 48rem;
  margin: 0.5rem auto 0;
  padding-left: 0.2rem;
  color: var(--text-muted);
  font-size: 0.75rem;
}

.chat-more-wrap { position: relative; flex: none; }
.chat-more-wrap .ev-more { width: 1.6rem; height: 1.6rem; font-size: 0.9rem; }
```

Extend the existing `.turn.assistant { width: 100% }` rule rather than duplicating it, and add `.receipt-chip` to the coarse-pointer `min-height: 2.5rem` list and to the `prefers-reduced-motion` transition-off list. Delete the dead `.tool`, `.tool-*`, `.chat-tools`, and `.chat-tool` rules along with their references in the two media blocks.

- [ ] **Step 7: Typecheck and build**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: both succeed.

- [ ] **Step 8: Browser verification against a stub API**

Serve a stub that returns canned `/api/me`, `/api/conversations`, and `/api/conversations/:id/messages` (including a successful and a failed tool message), logging every request. Drive it headless (`chromium --headless=new --remote-debugging-port`) and confirm:
- the default rendering of every tool message is a chip; expanding shows the raw name/args/result;
- the failed call shows a clay `✕` and its error inside;
- Delete needs `⋯` then `Delete`, and no `DELETE /api/conversations/:id` appears in the request log until 10 s later — and never, if Undo is pressed.

- [ ] **Step 9: Commit**

```bash
git add web/src/views/Talk.tsx web/src/app.tsx web/src/styles.css
git commit -m "feat: tool-call receipts and undoable chat delete"
```

---

## Self-review

**Spec coverage:**
- 8.1 chip + chevron + expandable raw card, clay `✕` for failures → Task 4 steps 1, 6.
- 8.2 per-tool template map covering all 11 tools, unknown-tool fallback → Task 1.
- 8.3 standing line under the composer → Task 4 step 4.
- 8.4 rename/delete behind `⋯`, 10 s deferred delete with Undo toast → Tasks 2, 3, 4 step 5.
- 8.5 assistant avatar + measured column, nothing else restyled → Task 4 step 3.
- Global constraints: only existing tokens; `--clay` (not `--danger`) for failures; focus-visible on chip and menu; coarse-pointer hit targets; reduced-motion respected.

**Type consistency:** `receipt(name, args, isError)` is called with `ToolItem` fields in Task 4 exactly as declared in Task 1. `Overflow` is called with `{ label, items }` in Today and `{ className, label, items }` in Talk, matching Task 2's signature. `ToastAction.windowMs` is written in Task 3 and read in Task 4.

**Out of scope, deliberately:** the step 9 placeholder string, the `.turn.system` error styling (pre-existing, not a tool receipt), and any conversation-list restyling beyond replacing the two text buttons.
