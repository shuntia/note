# Daylight steps 4 + 5 — Tasks view Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the flat Tasks list with the Daylight three-group view (NOW / LATER / DONE TODAY), in-place undo for completion and flatten, plus duration chips, rail-connected step lists, and the Start button.

**Architecture:** `web/src/views/Tasks.tsx` becomes a small component tree over one piece of state: the `TaskNode[]` the server returns. Grouping, counts, and empty states are all derived at render time, so an optimistic field write is the only mutation primitive. Every write applies locally first, fires its request, then merges the authoritative response (including `parent` cascades and `demoted_from_now`) back into the same array. Undo is not a client-side rollback — it is a second write that restores a captured snapshot.

**Tech Stack:** React 19, TypeScript 5.8, Vite 7, plain CSS with the existing tokens in `web/src/styles.css`. No new dependencies.

**Spec:** `docs/superpowers/plans/2026-09-01-daylight-ui-spec.md` (steps 4 and 5.3/5.4).
Server halves already shipped: `docs/superpowers/plans/2026-09-01-daylight-step-4a-now-flag.md`, `docs/superpowers/plans/2026-09-01-daylight-step-5a-task-model.md`.
Visual truth: `docs/superpowers/mockups/daylight/tasks-desktop-light.html`.

## Global Constraints

- Existing design tokens only. Accent = amber (`--sun`, `--sun-ink`) only; `--moss` = done; `--clay` = dropped/warn. **Never introduce red.** No new fonts, no new global tokens (that is step 10).
- The mockup's `--faint` third grey fails 4.5:1; follow the precedent set by the Today block in `styles.css` and resolve both `--quiet` and `--faint` to `var(--text-muted)`.
- Copy strings verbatim, em dashes included: `NOW · <k> of 3`, `a short list you can actually finish`, `LATER · <n>`, `DONE TODAY · <n>`, `Add a task — just a title is enough`, `press Enter to save`, `Now is full — finish or move something first`, `Nothing queued — pull something up from Later, or just add what's on your mind.`, `≈ 20 min`, `Note will estimate`, `≈ <total> min · Note split this into <n> steps · <k> done`, `in progress`.
- Sentence case, active voice, user vocabulary. An action keeps its name through its whole flow.
- Every state-changing action gets feedback within 100 ms (optimistic) and, where destructive, an in-place Undo with a 5 s window.
- Accessibility floor: visible `:focus-visible` on every interactive element; `prefers-reduced-motion` disables decorative animation; hit targets ≥ 40×40 px on coarse pointers; text contrast ≥ 4.5:1.
- Do not write anything under `server/`. Do not run `cargo`.

## Server contract (verified against `server/src/tasks.rs` and `server/src/api.rs`)

- `GET /api/tasks` → `TaskNode[]`, top-level only, `id ASC`. `children` always present, `id ASC`, dropped excluded, and children never nest further.
- `POST /api/tasks` `{title, duration_min?, parent_id?, is_now?}` → a plain `Task` with **no** `children` key.
- `PATCH /api/tasks/{id}` → `Task` fields, plus `parent: Task` **only when the key is present**, plus `demoted_from_now: number[]` (newest first) **only when non-empty**. No `children` key.
- `POST /api/tasks/{id}/split` `{steps:[{title,duration_min},…]}` (2–5) → `TaskNode`.
- `POST /api/tasks/{id}/flatten` (no body) → `{task: TaskNode, removed: Task[]}`.
- All request bodies are `deny_unknown_fields`: only ever send keys the server declares.
- `409` `{"error":"Now already holds 3 tasks"}` on an explicit user `is_now:true` that would be the 4th — **nothing moves**, and the client shows the spec's copy, not the server string.
- `422` `{"error":…}` for hierarchy/duration problems. `400`, `404`, `500` carry **no body**.
- `trim_now` runs after *every* successful write, so `demoted_from_now` can appear on a patch that never mentioned `is_now`.
- A task keeps `is_now` through `done`; the cap only counts `is_now && parent_id == null && state in (open, in_progress)`. That is why undo of a completion sends `{state}` **alone**.

---

## File Structure

- **Modify `web/src/types.ts`** — `Task` gains the five shipped fields; add `TaskNode`, `TaskUpdate`, `FlattenResult`, `NewStep`.
- **Modify `web/src/api.ts`** — `tasks()` retypes to `TaskNode[]`; `addTask` takes options; `patchTask` takes a patch object; add `splitTask`, `flattenTask`.
- **Rewrite `web/src/views/Tasks.tsx`** — the whole view: grouping, rows, steps, menus, undo. One file, because every piece shares the one `nodes` array and the one set of write helpers; splitting it would only move props around.
- **Modify `web/src/styles.css`** — replace the `/* tasks */` block with the Daylight group/row/step/chip styles; extend the coarse-pointer and reduced-motion blocks.

No other file changes. `web/src/app.tsx` already provides `notify(msg, {label, run, windowMs})` and `web/src/overflow.tsx` already provides the keyboard-operable `⋯` menu; both are reused as-is.

---

### Task 1: Types and API surface

**Files:**
- Modify: `web/src/types.ts:13-20`
- Modify: `web/src/api.ts:76-80`

**Interfaces:**
- Produces: `TaskNode`, `TaskUpdate`, `FlattenResult`, `NewStep`, `TaskPatch`; `api.tasks`, `api.addTask`, `api.patchTask`, `api.splitTask`, `api.flattenTask`.

- [ ] **Step 1: Replace the `Task` type and add the new ones**

```ts
export type TaskState = 'open' | 'in_progress' | 'done' | 'dropped'

export type Task = {
  id: number
  title: string
  description: string
  state: TaskState
  source: string
  notes: string
  duration_min: number | null
  duration_source: 'user' | 'agent' | 'none'
  parent_id: number | null
  is_now: boolean
  updated_at: string
}

// A top-level task with its steps; the list endpoint never nests deeper.
export type TaskNode = Task & { children: Task[] }

// `parent` arrives when finishing or reopening cascaded to it; `demoted_from_now`
// when the write pushed other tasks out of Now, newest first.
export type TaskUpdate = Task & { parent?: Task; demoted_from_now?: number[] }

export type NewStep = { title: string; duration_min: number }

export type FlattenResult = { task: TaskNode; removed: Task[] }
```

- [ ] **Step 2: Widen the four task calls in `api.ts`**

```ts
  tasks: () => request<TaskNode[]>('/api/tasks'),
  addTask: (title: string, opts?: { duration_min?: number; parent_id?: number; is_now?: boolean }) =>
    request<Task>('/api/tasks', { method: 'POST', body: JSON.stringify({ title, ...opts }) }),
  // Undefined keys drop out of the body, which the server requires: it rejects
  // unknown fields and reads an explicit null as "clear it".
  patchTask: (id: number, patch: TaskPatch) =>
    request<TaskUpdate>(`/api/tasks/${id}`, { method: 'PATCH', body: JSON.stringify(patch) }),
  splitTask: (id: number, steps: NewStep[]) =>
    request<TaskNode>(`/api/tasks/${id}/split`, {
      method: 'POST',
      body: JSON.stringify({ steps }),
    }),
  flattenTask: (id: number) =>
    request<FlattenResult>(`/api/tasks/${id}/flatten`, { method: 'POST' }),
```

with, above `export const api`:

```ts
type TaskPatch = { state?: TaskState; is_now?: boolean }
```

Import `NewStep`, `TaskNode`, `TaskState`, `TaskUpdate`, `FlattenResult` alongside the existing `Task`.

- [ ] **Step 3: Typecheck**

Run: `cd web && npx tsc --noEmit`
Expected: two errors in `views/Tasks.tsx` (`api.patchTask` now wants an object, `tasks` is `TaskNode[]`). Task 2 clears them.

---

### Task 2: The Tasks view — grouping, rows, and undo (step 4)

**Files:**
- Rewrite: `web/src/views/Tasks.tsx`
- Modify: `web/src/styles.css:708-767` (the `/* tasks */` block) and the coarse-pointer block at `:684` and reduced-motion block at `:764`

**Interfaces:**
- Consumes: `api.tasks`, `api.addTask`, `api.patchTask` from Task 1; `notify` and `refresh` from `ViewProps`; `Overflow` from `../overflow`.
- Produces: `groupOf`, `Snapshot`, `restore`, `mergeUpdate` — Task 3 hangs the step list, duration chips, and Start button on the same row components.

- [ ] **Step 1: Derived grouping**

Grouping is a pure function of the loaded array, so no group ever needs its own state.

```ts
const NOW_CAP = 3
const UNDO_MS = 5000

const isLive = (t: Task) => t.state === 'open' || t.state === 'in_progress'

function sameDay(iso: string, today: Date): boolean {
  const d = new Date(iso)
  return (
    d.getFullYear() === today.getFullYear() &&
    d.getMonth() === today.getMonth() &&
    d.getDate() === today.getDate()
  )
}

// The server caps Now itself, but a stale frame between an agent write and the
// reload must not render a fourth: the newest live members overflow into Later,
// which is the order the server demotes in.
function groups(nodes: TaskNode[]) {
  const today = new Date()
  const live = nodes.filter(isLive)
  const now = live.filter((n) => n.is_now).slice(0, NOW_CAP)
  const inNow = new Set(now.map((n) => n.id))
  return {
    now,
    later: live.filter((n) => !inNow.has(n.id)),
    doneToday: nodes.filter((n) => n.state === 'done' && sameDay(n.updated_at, today)),
  }
}
```

- [ ] **Step 2: Local merge helpers**

```ts
type Snapshot = { id: number; state: TaskState }[]

function withTask(nodes: TaskNode[], t: Task): TaskNode[] {
  return nodes.map((n) => {
    if (n.id === t.id) return { ...n, ...t, children: n.children }
    if (!n.children.some((c) => c.id === t.id)) return n
    return { ...n, children: n.children.map((c) => (c.id === t.id ? { ...c, ...t } : c)) }
  })
}

function withState(nodes: TaskNode[], id: number, state: TaskState): TaskNode[] {
  const stamp = new Date().toISOString()
  return nodes.map((n) => {
    if (n.id === id) return { ...n, state, updated_at: stamp }
    if (!n.children.some((c) => c.id === id)) return n
    return {
      ...n,
      children: n.children.map((c) => (c.id === id ? { ...c, state, updated_at: stamp } : c)),
    }
  })
}

// `demoted_from_now` can land on any patch, not only one that set `is_now`.
function mergeUpdate(nodes: TaskNode[], u: TaskUpdate): TaskNode[] {
  const { parent, demoted_from_now, ...task } = u
  let next = withTask(nodes, task)
  if (parent) next = withTask(next, parent)
  if (demoted_from_now?.length) {
    const out = new Set(demoted_from_now)
    next = next.map((n) => (out.has(n.id) ? { ...n, is_now: false } : n))
  }
  return next
}
```

- [ ] **Step 3: The write primitives**

One optimistic-then-reconcile helper covers every state write; undo replays a snapshot through the same helper.

```ts
export function Tasks({ notify, refresh }: ViewProps) {
  const [nodes, setNodes] = useState<TaskNode[] | null>(null)
  const [failed, setFailed] = useState(false)
  const [title, setTitle] = useState('')

  const load = useCallback(() => {
    api
      .tasks()
      .then((ts) => {
        setNodes(ts)
        setFailed(false)
      })
      .catch(() => setFailed(true))
  }, [])
  useEffect(load, [load, refresh])

  const patch = useCallback(
    async (id: number, body: { state?: TaskState; is_now?: boolean }) => {
      try {
        const updated = await api.patchTask(id, body)
        setNodes((ns) => (ns ? mergeUpdate(ns, updated) : ns))
        return true
      } catch (err) {
        if (err instanceof ApiError && err.status === 409) {
          notify('Now is full — finish or move something first')
        } else {
          notify("Couldn't update the task. Try again.")
        }
        load()
        return false
      }
    },
    [load, notify],
  )

  // Steps first: reopening one cascades the parent, so the parent's own state
  // has to land last to win.
  const restore = useCallback(
    async (snap: Snapshot) => {
      setNodes((ns) => (ns ? snap.reduce((acc, s) => withState(acc, s.id, s.state), ns) : ns))
      for (const s of snap) await patch(s.id, { state: s.state })
    },
    [patch],
  )
```

- [ ] **Step 4: Completion with undo**

Completing a parent cascades its live steps to done, so the snapshot has to carry them or undo would leave the steps ticked. `complete` handles both a top-level task and a step; `finished` names the row whose completion the toast reports.

```ts
  const complete = (node: TaskNode, step?: Task) => {
    const target = step ?? node
    const lastStep =
      step !== undefined && node.children.every((c) => c.id === step.id || c.state === 'done')
    const cascaded = step
      ? lastStep
        ? [{ id: node.id, state: node.state }]
        : []
      : node.children.filter((c) => c.state !== 'done').map((c) => ({ id: c.id, state: c.state }))
    const snap: Snapshot = [
      ...cascaded.filter((s) => s.id !== node.id),
      { id: target.id, state: target.state },
      ...cascaded.filter((s) => s.id === node.id),
    ]

    setNodes((ns) => {
      if (!ns) return ns
      let next = withState(ns, target.id, 'done')
      for (const c of cascaded) next = withState(next, c.id, 'done')
      return next
    })
    void patch(target.id, { state: 'done' })

    const finished = step && !lastStep ? step.title : node.title
    notify(`${finished} — done`, {
      label: 'Undo',
      run: () => void restore(snap),
      windowMs: UNDO_MS,
    })
  }
```

Reopening from DONE TODAY takes the steps back with the parent, so the sub-line's `<k> done` cannot disagree with an open parent:

```ts
  const reopen = (node: TaskNode) =>
    void restore([
      ...node.children.map((c) => ({ id: c.id, state: 'open' as TaskState })),
      { id: node.id, state: 'open' as TaskState },
    ])
```

- [ ] **Step 5: Now / Later moves and the in-progress toggle**

A local count check keeps a refusal instant and avoids a visible move-then-snap-back; the 409 handler in `patch` still covers a stale count.

```ts
  const moveToNow = (node: TaskNode, nowFull: boolean) => {
    if (nowFull) return notify('Now is full — finish or move something first')
    setNodes((ns) => (ns ? ns.map((n) => (n.id === node.id ? { ...n, is_now: true } : n)) : ns))
    void patch(node.id, { is_now: true })
  }

  const moveToLater = (node: TaskNode) => {
    setNodes((ns) => (ns ? ns.map((n) => (n.id === node.id ? { ...n, is_now: false } : n)) : ns))
    void patch(node.id, { is_now: false })
  }

  const setProgress = (node: TaskNode, on: boolean) => {
    const state: TaskState = on ? 'in_progress' : 'open'
    setNodes((ns) => (ns ? withState(ns, node.id, state) : ns))
    void patch(node.id, { state })
  }
```

Menu labels: `Move to Now` / `Move to Later` (spec), and `Mark in progress` / `Clear in progress` for the toggle — the state keeps the name the row shows.

- [ ] **Step 6: The add row**

```ts
  const add = (e: FormEvent) => {
    e.preventDefault()
    const text = title.trim()
    if (!text || !nodes) return
    setTitle('')
    const g = groups(nodes)
    // A first task lands where the user will look for it, if Now has the room.
    const is_now = g.now.length === 0 && g.later.length === 0
    const optimistic: TaskNode = {
      id: -Date.now(), title: text, description: '', state: 'open', source: 'manual',
      notes: '', duration_min: null, duration_source: 'none', parent_id: null,
      is_now, updated_at: new Date().toISOString(), children: [],
    }
    setNodes((ns) => (ns ? [...ns, optimistic] : ns))
    api
      .addTask(text, is_now ? { is_now: true } : undefined)
      .then((t) =>
        setNodes((ns) =>
          ns ? ns.map((n) => (n.id === optimistic.id ? { ...n, ...t, children: [] } : n)) : ns,
        ),
      )
      .catch(() => {
        setNodes((ns) => (ns ? ns.filter((n) => n.id !== optimistic.id) : ns))
        notify("Couldn't add the task. Try again.")
      })
  }
```

Markup, per the mockup's `.add` card:

```tsx
      <form className="task-add" onSubmit={add}>
        <span className="task-add-glyph" aria-hidden="true">＋</span>
        <input
          value={title}
          placeholder="Add a task — just a title is enough"
          aria-label="Add a task — just a title is enough"
          onChange={(e) => setTitle(e.target.value)}
        />
        <span className="task-add-hint">press Enter to save</span>
      </form>
```

- [ ] **Step 7: Render the three groups and the empty states**

```tsx
  if (failed)
    return (
      <div className="page tasks">
        <p className="muted">
          Couldn't load tasks.{' '}
          <button className="quiet" onClick={load}>Retry</button>
        </p>
      </div>
    )
  if (!nodes) return null

  const g = groups(nodes)
  const empty = g.now.length === 0 && g.later.length === 0 && g.doneToday.length === 0

  return (
    <div className="page tasks">
      <SectionTitle>Tasks</SectionTitle>
      {/* add form from step 6 */}
      {empty ? (
        <p className="task-empty">{NOW_EMPTY}</p>
      ) : (
        <>
          <section className="task-group now">
            <h3 className="task-group-head">
              NOW <span className="task-group-count">· {g.now.length} of {NOW_CAP}</span>
              <span className="task-group-why">a short list you can actually finish</span>
            </h3>
            {g.now.length === 0 ? (
              <p className="task-empty">{NOW_EMPTY}</p>
            ) : (
              g.now.map((n) => <Row key={n.id} node={n} group="now" … />)
            )}
          </section>
          {g.later.length > 0 && (
            <section className="task-group later">
              <h3 className="task-group-head">LATER <span className="task-group-count">· {g.later.length}</span></h3>
              {g.later.map((n) => <Row key={n.id} node={n} group="later" … />)}
            </section>
          )}
          {g.doneToday.length > 0 && (
            <section className="task-group done">
              <h3 className="task-group-head">DONE TODAY <span className="task-group-count">· {g.doneToday.length}</span></h3>
              {g.doneToday.map((n) => <Row key={n.id} node={n} group="done" … />)}
            </section>
          )}
        </>
      )}
    </div>
  )
```

with, at module scope:

```ts
const NOW_EMPTY = "Nothing queued — pull something up from Later, or just add what's on your mind."
```

- [ ] **Step 8: The row**

```tsx
function Row({ node, group, nowFull, actions }: RowProps) {
  const done = group === 'done'
  const items: OverflowItem[] = [
    group === 'now'
      ? { label: 'Move to Later', run: () => actions.moveToLater(node) }
      : { label: 'Move to Now', run: () => actions.moveToNow(node, nowFull) },
    node.state === 'in_progress'
      ? { label: 'Clear in progress', run: () => actions.setProgress(node, false) }
      : { label: 'Mark in progress', run: () => actions.setProgress(node, true) },
  ]
  return (
    <div className="task-row">
      <div className="task-head">
        <button
          className="task-tick"
          aria-label={done ? `Mark ${node.title} not done` : `Mark ${node.title} done`}
          onClick={() => (done ? actions.reopen(node) : actions.complete(node))}
        >
          <span className="tick" aria-hidden="true" />
        </button>
        <div className="task-body">
          <div className="task-title">{node.title}</div>
        </div>
        {!done && node.state === 'in_progress' && <span className="task-tag progress">in progress</span>}
        {!done && <Overflow className="task-more" label={`More actions for ${node.title}`} items={items} />}
      </div>
    </div>
  )
}
```

Done rows carry no menu — the group is a receipt, and its one affordance is the circle.

- [ ] **Step 9: Styles**

Replace the `/* tasks */` block in `styles.css`. Values follow the mockup; `--faint`/`--quiet` resolve to `--text-muted`, `--mist` to `--border`, `--card` to `--surface`.

```css
/* tasks — three groups down one reading column: Now is card-weighted, Later is
   the same shape gone quiet, Done Today is a receipt. */
.tasks { max-width: 45rem; }

.task-add {
  display: flex; align-items: center; gap: 0.65rem;
  background: var(--surface); border: 1px solid var(--border);
  border-radius: var(--radius-lg); padding: 0.7rem 1rem; margin: 0 0 1.75rem;
  transition: border-color 150ms ease, box-shadow 150ms ease;
}
.task-add:focus-within { border-color: var(--ring); box-shadow: 0 0 0 1px var(--ring); }
.task-add-glyph { flex: none; color: var(--sun-ink); line-height: 1; }
.task-add input { flex: 1; min-width: 0; padding: 0; background: none; border: none; }
.task-add input:focus-visible { outline: none; }
.task-add-hint { flex: none; font-size: 0.78rem; color: var(--text-muted); }

.task-group { margin-bottom: 1.65rem; }
.task-group-head {
  display: flex; align-items: baseline; gap: 0.5rem; margin: 0 0 0.6rem;
  font-size: 0.78rem; font-weight: 700; letter-spacing: 0.09em; color: var(--text-muted);
}
.task-group-count { font-weight: 400; letter-spacing: 0; }
.task-group-why { margin-left: auto; font-weight: 400; letter-spacing: 0; font-size: 0.75rem; }

.task-row {
  background: var(--surface); border: 1px solid var(--border);
  border-radius: var(--radius-lg); padding: 0.8rem 1rem; margin-bottom: 0.5rem;
  transition: border-color 120ms ease;
}
.task-group.now .task-row { box-shadow: 0 6px 18px -14px color-mix(in srgb, var(--text) 60%, transparent); }
.task-row:hover { border-color: var(--border-input); }
.task-head { display: flex; align-items: center; gap: 0.8rem; }

.task-tick {
  flex: none; display: grid; place-items: center;
  width: 1.4rem; height: 1.4rem; padding: 0; background: none; border: none; cursor: pointer;
}
.tick {
  width: 1.375rem; height: 1.375rem; border-radius: 50%;
  border: 2px solid var(--border-input); transition: border-color 120ms ease;
}
.task-row:hover .tick { border-color: var(--sun); }

.task-body { min-width: 0; }
.task-title { color: var(--text); }
.task-sub { color: var(--text-muted); font-size: 0.78rem; }

.task-tag {
  flex: none; margin-left: auto; font-size: 0.72rem;
  border: 1px solid var(--border); border-radius: 999px; padding: 0.05rem 0.55rem;
  color: var(--text-muted);
}
.task-tag.progress {
  color: var(--sun-ink);
  border-color: color-mix(in srgb, var(--sun) 35%, var(--border));
  background: color-mix(in srgb, var(--sun) 12%, var(--surface));
}
.task-more { position: relative; flex: none; margin-left: auto; }
.task-tag + .task-more, .task-dur + .task-more, .task-start + .task-more { margin-left: 0; }

.task-group.later .task-row { background: none; box-shadow: none; }
.task-group.later .task-title { color: var(--text-muted); }

.task-group.done .task-row { background: none; border-color: transparent; padding-block: 0.35rem; }
.task-group.done .tick { border-color: var(--moss); background: var(--moss); }
.task-group.done .tick::after { /* the check glyph, drawn as a rotated border */ }
.task-group.done .task-title {
  color: var(--text-muted); text-decoration: line-through;
  text-decoration-color: color-mix(in srgb, var(--moss) 45%, transparent);
}

.task-empty { margin: 0 0 0.5rem; color: var(--text-muted); }
```

The check glyph reuses the `.check::after` rotated-border technique already in the file rather than an inline SVG data URI, so it inherits the theme.

Extend the existing coarse-pointer block with `.task-tick, .task-start { min-width: 2.5rem; min-height: 2.5rem; }` and the existing reduced-motion block with `.task-add, .task-row, .tick, .task-start`.

- [ ] **Step 10: Verify**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: both clean.

- [ ] **Step 11: Commit step 4**

```bash
git add web/src/types.ts web/src/api.ts web/src/views/Tasks.tsx web/src/styles.css \
        docs/superpowers/plans/2026-09-01-daylight-step-4-5-tasks-ui.md
git commit -m "feat: task groups and undo"
```

---

### Task 3: Durations, steps, and the Start button (step 5.3 + 5.4)

**Files:**
- Modify: `web/src/views/Tasks.tsx`
- Modify: `web/src/styles.css` (the `/* tasks */` block from Task 2)

**Interfaces:**
- Consumes: `Row`, `complete`, `patch`, `restore` from Task 2; `api.splitTask`, `api.flattenTask` from Task 1.
- Produces: `durationText`, `focusTarget`, `keepAsOne`, `start`.

- [ ] **Step 1: Duration text**

The server only accepts multiples of 5, but the display rounds anyway so no future write path can leak a stray minute into the UI.

```ts
const round5 = (min: number) => Math.max(5, Math.round(min / 5) * 5)
```

Chip rendering, per the mockup: solid outline `≈ <d> min` when `duration_min` is set; dashed outline `Note will estimate` when it is null and `duration_source === 'none'`; nothing at all otherwise.

```tsx
function Duration({ task }: { task: Task }) {
  if (task.duration_min !== null) return <span className="task-dur">≈ {round5(task.duration_min)} min</span>
  if (task.duration_source !== 'none') return null
  return <span className="task-dur est">Note will estimate</span>
}
```

Steps use the bare form the mockup shows: `<span className="task-dur">{round5(c.duration_min)} min</span>`, rendered only when the step has a duration.

- [ ] **Step 2: The parent sub-line and step list**

A parent's total is its own `duration_min` (the split sets it to the sum), and `<k> done` counts live children in `done`.

```tsx
function parentSub(node: TaskNode): string {
  const done = node.children.filter((c) => c.state === 'done').length
  const parts = node.duration_min !== null ? [`≈ ${round5(node.duration_min)} min`] : []
  parts.push(`Note split this into ${node.children.length} steps`, `${done} done`)
  return parts.join(' · ')
}
```

The step list renders under the head, rail-connected, only for rows outside DONE TODAY. `next` is the first live step — the one the Start button would open.

```tsx
      {node.children.length > 0 && !done && (
        <div className="task-steps">
          {node.children.map((c) => (
            <div
              key={c.id}
              className={`task-step${c.state === 'done' ? ' done' : ''}${c.id === next?.id ? ' next' : ''}`}
            >
              <button
                className="task-tick"
                aria-label={c.state === 'done' ? `Mark ${c.title} not done` : `Mark ${c.title} done`}
                onClick={() =>
                  c.state === 'done'
                    ? actions.reopenStep(node, c)
                    : actions.complete(node, c)
                }
              >
                <span className="ticksm" aria-hidden="true" />
              </button>
              <span className="task-step-title">{c.title}</span>
              {c.duration_min !== null && <span className="task-dur">{round5(c.duration_min)} min</span>}
            </div>
          ))}
        </div>
      )}
```

`reopenStep` is a one-liner on the existing primitive — the server's cascade moves the parent back and `mergeUpdate` picks it up from the response's `parent` key:

```ts
  const reopenStep = (node: TaskNode, step: Task) => void restore([{ id: step.id, state: 'open' }])
```

`complete(node, step)` from Task 2 already flips the parent optimistically when the step is the last live one, which is the acceptance criterion.

- [ ] **Step 3: `Keep as one task`**

Flatten is destructive and reversible only by replaying the split, so the removed steps travel into the undo closure.

```ts
  const keepAsOne = (node: TaskNode) => {
    const steps: NewStep[] = node.children
      .filter((c) => c.duration_min !== null)
      .map((c) => ({ title: c.title, duration_min: c.duration_min as number }))
    setNodes((ns) => (ns ? ns.map((n) => (n.id === node.id ? { ...n, children: [] } : n)) : ns))
    api
      .flattenTask(node.id)
      .then((r) => setNodes((ns) => (ns ? withTask(ns, r.task).map(
        (n) => (n.id === r.task.id ? { ...n, children: r.task.children } : n)) : ns)))
      .catch(() => {
        notify("Couldn't update the task. Try again.")
        load()
      })
    notify(`${node.title} — kept as one task`, {
      label: 'Undo',
      run: () => {
        api
          .splitTask(node.id, steps)
          .then((n) => setNodes((ns) => (ns ? ns.map((x) => (x.id === n.id ? n : x)) : ns)))
          .catch(() => {
            notify("Couldn't put the steps back. Try again.")
            load()
          })
      },
      windowMs: UNDO_MS,
    })
  }
```

The menu item is appended to `items` in `Row` only when `node.children.length > 0`:

```ts
  if (node.children.length > 0) items.push({ label: 'Keep as one task', run: () => actions.keepAsOne(node) })
```

- [ ] **Step 4: The Start button**

```tsx
// The step a session would open on: a parent hands off to its next live step.
function focusTarget(node: TaskNode): Task {
  return node.children.find((c) => c.state !== 'done') ?? node
}
```

Rendered on NOW-group rows only, before the `⋯`:

```tsx
        {group === 'now' && (
          <button className="task-start" aria-label={`Start ${node.title}`} onClick={() => actions.start(node)}>
            <span aria-hidden="true">▶</span>
          </button>
        )}
```

The Now screen is step 6 and does not exist. Until it does, `start` does the one real thing "starting" means in this model — it marks the task in progress, which is the same transition the `⋯` menu offers, and gives the row its `in progress` tag inside the same frame. Step 6 replaces the body of this one function with the navigation, keeping `focusTarget` as the task it hands over.

```ts
  const start = (node: TaskNode) => {
    focusTarget(node)
    if (node.state !== 'in_progress') setProgress(node, true)
  }
```

- [ ] **Step 5: Styles for chips, steps, and the Start button**

```css
.task-dur {
  flex: none; margin-left: auto; white-space: nowrap;
  font-size: 0.72rem; color: var(--text-muted);
  border: 1px solid var(--border); border-radius: 999px; padding: 0.05rem 0.5rem;
}
.task-dur.est { border-style: dashed; }

.task-start {
  flex: none; display: grid; place-items: center;
  width: 2.125rem; height: 2.125rem; padding-left: 2px;
  border-radius: 50%; background: var(--surface);
  border: 1px solid color-mix(in srgb, var(--sun) 40%, var(--border));
  color: var(--sun-ink); font-size: 0.8rem; cursor: pointer;
  transition: background 120ms ease, border-color 120ms ease;
}
.task-start:hover { background: color-mix(in srgb, var(--sun) 10%, var(--surface)); border-color: var(--sun); }

.task-steps {
  display: grid; gap: 0.35rem;
  margin: 0.6rem 0 0.1rem 2.2rem; padding-left: 1.1rem;
  border-left: 2px solid var(--border);
}
.task-step { display: flex; align-items: center; gap: 0.6rem; font-size: 0.9rem; color: var(--text-muted); }
.task-step-title { min-width: 0; }
.ticksm { width: 1rem; height: 1rem; border-radius: 50%; border: 2px solid var(--border-input); }
.task-step.done .ticksm { border-color: var(--moss); background: var(--moss); }
.task-step.done .task-step-title {
  text-decoration: line-through;
  text-decoration-color: color-mix(in srgb, var(--moss) 45%, transparent);
}
.task-step.next .task-step-title { color: var(--text); font-weight: 700; }
```

The moss check inside `.ticksm` and `.tick` shares one rule, so both sizes draw the same glyph.

- [ ] **Step 6: Verify**

Run: `cd web && npx tsc --noEmit && npx vite build`
Expected: both clean.

- [ ] **Step 7: Commit step 5**

```bash
git add web/src/views/Tasks.tsx web/src/styles.css
git commit -m "feat: task durations, steps, and start"
```

---

### Task 4: Browser acceptance run

**Files:**
- Create (scratchpad only, never committed): a static server with an in-memory mock of the four task endpoints, and a CDP driver script.

There is no JS test runner in this repo, and the acceptance criteria are all rendering and interaction facts, so they are checked against the real built bundle in a real browser. Playwright's bundled Chromium cannot start on this host (`libgbm.so.1`); the system `chromium` is driven over CDP with Node 24's global `WebSocket`.

- [ ] **Step 1: Build, then serve `web/dist` with a mock API**

The mock mirrors `server/src/tasks.rs`: `list` returns top-level rows in id order with their children; `create` honours `is_now` and trims; `update` cascades parent↔step and runs `trim_now`, returning `parent` and `demoted_from_now`; a user `is_now:true` over the cap answers `409 {"error":"Now already holds 3 tasks"}`; `split`/`flatten` behave as shipped.

- [ ] **Step 2: Drive the acceptance list**

Launch: `chromium --headless=new --remote-debugging-port=<p> --user-data-dir=<scratch>`, then `Page.navigate`, `Runtime.evaluate` for assertions, `Input.dispatchMouseEvent` for clicks.

Walk each box, capturing the rendered DOM as evidence:
- Now never renders more than 3 tasks, including after an agent write that leaves 4 flagged (the 4th falls into Later).
- Done + Undo round-trips a task to its exact prior group and state.
- Empty Now shows the spec sentence; a fully empty view shows only the add row and that line.
- `Keep as one` removes children in one action and is undoable.
- Durations only ever display in 5-minute increments.
- Completing the last child marks the parent done in the same optimistic frame.
- The 4th `Move to Now` shows `Now is full — finish or move something first` and does not move the row.
- Keyboard: the `⋯` menu opens and operates from the keyboard, and the toast's Undo is focusable.

---

## Self-review

**Spec coverage.** Step 4.1 groups, headers, hints, caps, `⋯` moves → Task 2 steps 1, 5, 7, 8. 4.2 completion + undo → Task 2 step 4. 4.3 add row → Task 2 step 6. 4.4 in-progress tag → Task 2 steps 5, 8. 5.3 duration chips, parent sub-line, step list, all-children completion → Task 3 steps 1–2. 5.2's `Keep as one task` (the flatten half, which lives in this view) → Task 3 step 3. 5.4 Start button → Task 3 step 4. Global accessibility floor → Task 2 step 9 and Task 3 step 5. 5.1 and the agent's split/duration tools are server-side and already shipped; 5.4's navigation target is step 6 and is deliberately left as the one named seam.

**Known imprecision.** DONE TODAY filters on `updated_at`, which is a last-changed stamp, not a completion stamp. The server exposes nothing better. A task finished yesterday and touched today (an agent note, a duration estimate, a `trim_now` demotion) will appear in DONE TODAY. This is recorded here rather than hidden.

**Deliberate omissions.** The mockup's `added 2 days ago` sub-line on a plain row has no spec copy behind it and is not implemented. The orange `◆` on the view title stays until step 10 removes it app-wide.
