import { useCallback, useEffect, useState, type FormEvent } from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { Overflow, type OverflowItem } from '../overflow'
import type { NewStep, Task, TaskNode, TaskState, TaskUpdate } from '../types'

const NOW_CAP = 3
const UNDO_MS = 5000
const NOW_FULL = 'Now is full — finish or move something first'

type Group = 'now' | 'later' | 'done'

// The prior states a completion has to put back, steps first: reopening a step
// cascades the parent, so the parent's own state must land last to win.
type Snapshot = { id: number; state: TaskState }[]

const isLive = (t: Task) => t.state === 'open' || t.state === 'in_progress'

// The server only stores multiples of five; rounding here means no display path
// can ever show a duration the user could not have been offered.
const round5 = (min: number) => Math.max(5, Math.round(min / 5) * 5)

// A parent hands its focus session off to its next unfinished step.
const focusTarget = (node: TaskNode): Task =>
  node.children.find((c) => c.state !== 'done') ?? node

function parentSub(node: TaskNode): string {
  const done = node.children.filter((c) => c.state === 'done').length
  return `${node.children.length} steps · ${done} done`
}

function sameDay(iso: string, today: Date): boolean {
  const d = new Date(iso)
  return (
    d.getFullYear() === today.getFullYear() &&
    d.getMonth() === today.getMonth() &&
    d.getDate() === today.getDate()
  )
}

// The server caps Now itself, but a frame between an agent write and the reload
// must not render a fourth: the newest live members overflow into Later, which
// is the order the server demotes in.
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

// `demoted_from_now` rides on any write, not only one that set `is_now`.
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

type RowActions = {
  complete: (node: TaskNode, step?: Task) => void
  reopen: (node: TaskNode) => void
  reopenStep: (step: Task) => void
  moveToNow: (node: TaskNode) => void
  moveToLater: (node: TaskNode) => void
  drop: (node: TaskNode) => void
  keepAsOne: (node: TaskNode) => void
  startFocus: (node: TaskNode) => void
}

export function Tasks({ notify, refresh, openNow }: ViewProps) {
  const [nodes, setNodes] = useState<TaskNode[] | null>(null)
  const [failed, setFailed] = useState(false)
  const [title, setTitle] = useState('')
  const [showDone, setShowDone] = useState(false)

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
      } catch (err) {
        notify(
          err instanceof ApiError && err.status === 409
            ? NOW_FULL
            : "Couldn't update the task. Try again.",
        )
        load()
      }
    },
    [load, notify],
  )

  const restore = useCallback(
    async (snap: Snapshot) => {
      setNodes((ns) => (ns ? snap.reduce((acc, s) => withState(acc, s.id, s.state), ns) : ns))
      for (const s of snap) await patch(s.id, { state: s.state })
    },
    [patch],
  )

  // Finishing a task takes its live steps with it, and finishing the last step
  // finishes the task, so undo has to put the whole cascade back.
  const complete = (node: TaskNode, step?: Task) => {
    const lastStep =
      step !== undefined && node.children.every((c) => c.id === step.id || c.state === 'done')
    const cascade = step
      ? lastStep
        ? [node]
        : []
      : node.children.filter((c) => c.state !== 'done')
    const ordered = step ? [step, ...cascade] : [...cascade, node]
    const snap: Snapshot = ordered.map((t) => ({ id: t.id, state: t.state }))
    setNodes((ns) => (ns ? snap.reduce((acc, s) => withState(acc, s.id, 'done'), ns) : ns))
    void patch(step ? step.id : node.id, { state: 'done' })
    notify(`${step && !lastStep ? step.title : node.title} — done`, {
      label: 'Undo',
      run: () => void restore(snap),
      windowMs: UNDO_MS,
    })
  }

  // Reopening a finished task brings its steps back too, so the sub-line's count
  // cannot claim work is done under a task that is open again.
  const reopen = (node: TaskNode) =>
    void restore([
      ...node.children.map((c) => ({ id: c.id, state: 'open' as TaskState })),
      { id: node.id, state: 'open' as TaskState },
    ])

  const reopenStep = (step: Task) => void restore([{ id: step.id, state: 'open' }])

  // Dropping a task drops every step it still has, finished ones included, and a
  // dropped step never comes back on its own, so undo has to name each of them.
  const drop = (node: TaskNode) => {
    const steps = node.children.filter((c) => c.state !== 'dropped')
    const snap: Snapshot = [...steps, node].map((t) => ({ id: t.id, state: t.state }))
    setNodes((ns) => (ns ? snap.reduce((acc, s) => withState(acc, s.id, 'dropped'), ns) : ns))
    void patch(node.id, { state: 'dropped' })
    notify(`${node.title} — dropped`, {
      label: 'Undo',
      run: () => void restore(snap),
      windowMs: UNDO_MS,
    })
  }

  const setNow = (node: TaskNode, is_now: boolean) => {
    setNodes((ns) => (ns ? ns.map((n) => (n.id === node.id ? { ...n, is_now } : n)) : ns))
    void patch(node.id, { is_now })
  }

  // The only reversal a flatten has is replaying the split, so the steps travel
  // into the undo closure rather than being read back off a stale row.
  const keepAsOne = (node: TaskNode) => {
    const steps: NewStep[] = node.children
      .filter((c) => c.duration_min !== null)
      .map((c) => ({ title: c.title, duration_min: c.duration_min as number }))
    const put = (n: TaskNode) =>
      setNodes((ns) => (ns ? ns.map((x) => (x.id === n.id ? { ...x, ...n } : x)) : ns))
    setNodes((ns) => (ns ? ns.map((n) => (n.id === node.id ? { ...n, children: [] } : n)) : ns))
    api
      .flattenTask(node.id)
      .then((r) => put(r.task))
      .catch(() => {
        notify("Couldn't update the task. Try again.")
        load()
      })
    notify(`${node.title} — kept as one task`, {
      label: 'Undo',
      run: () =>
        void api
          .splitTask(node.id, steps)
          .then(put)
          .catch(() => {
            notify("Couldn't put the steps back. Try again.")
            load()
          }),
      windowMs: UNDO_MS,
    })
  }

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

  const add = (e: FormEvent) => {
    e.preventDefault()
    const text = title.trim()
    if (!text || !nodes) return
    setTitle('')
    const g = groups(nodes)
    const is_now = g.now.length === 0 && g.later.length === 0
    const optimistic: TaskNode = {
      id: -Date.now(),
      title: text,
      description: '',
      state: 'open',
      source: 'manual',
      notes: '',
      duration_min: null,
      duration_source: 'none',
      parent_id: null,
      is_now,
      updated_at: new Date().toISOString(),
      due_at: null,
      external_id: null,
      url: '',
      children: [],
    }
    setNodes((ns) => (ns ? [...ns, optimistic] : ns))
    // The row is live before the server answers, so undo may land either side of
    // that: the id it has to drop is the real one once there is one.
    let created: Task | null = null
    let undone = false
    const forget = (id: number) => setNodes((ns) => (ns ? ns.filter((n) => n.id !== id) : ns))
    api
      .addTask(text, is_now ? { is_now: true } : undefined)
      .then((t) => {
        created = t
        if (undone) {
          void patch(t.id, { state: 'dropped' })
          return
        }
        setNodes((ns) =>
          ns ? ns.map((n) => (n.id === optimistic.id ? { ...n, ...t, children: [] } : n)) : ns,
        )
      })
      .catch(() => {
        forget(optimistic.id)
        notify("Couldn't add the task. Try again.")
      })
    notify(`${text} — added`, {
      label: 'Undo',
      run: () => {
        undone = true
        forget(created ? created.id : optimistic.id)
        if (created) void patch(created.id, { state: 'dropped' })
      },
      windowMs: UNDO_MS,
    })
  }

  if (failed)
    return (
      <div className="tasks">
        <p className="muted">
          Couldn't load tasks.{' '}
          <button className="quiet" onClick={load}>
            Retry
          </button>
        </p>
      </div>
    )
  if (nodes === null) return null

  const g = groups(nodes)
  const actions: RowActions = {
    complete,
    reopen,
    reopenStep,
    moveToNow: (node) => (g.now.length >= NOW_CAP ? notify(NOW_FULL) : setNow(node, true)),
    moveToLater: (node) => setNow(node, false),
    drop,
    keepAsOne,
    startFocus,
  }

  return (
    <div className="tasks">
      <form className="task-add tellnote" onSubmit={add}>
        <input
          value={title}
          placeholder="Add a task"
          aria-label="Add a task"
          onChange={(e) => setTitle(e.target.value)}
        />
        <button type="submit" aria-label="Add" disabled={!title.trim()}>
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M5 12h14" />
            <path d="M13 6l6 6-6 6" />
          </svg>
        </button>
      </form>
      {g.now.length > 0 && (
        <section className="task-group now">
          <h3 className="task-group-head">NOW</h3>
          {g.now.map((n) => (
            <Row key={n.id} node={n} group="now" actions={actions} />
          ))}
        </section>
      )}
      {g.later.length > 0 && (
        <section className="task-group later">
          <h3 className="task-group-head">LATER · {g.later.length}</h3>
          {g.later.map((n) => (
            <Row key={n.id} node={n} group="later" actions={actions} />
          ))}
        </section>
      )}
      {g.doneToday.length > 0 && (
        <section className="task-group done">
          <button
            className="task-done-fold"
            aria-expanded={showDone}
            onClick={() => setShowDone((v) => !v)}
          >
            Done today · {g.doneToday.length}
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M9 6l6 6-6 6" />
            </svg>
          </button>
          {showDone &&
            g.doneToday.map((n) => <Row key={n.id} node={n} group="done" actions={actions} />)}
        </section>
      )}
    </div>
  )
}

function Duration({ task }: { task: Task }) {
  if (task.duration_min === null) return null
  return <span className="task-dur">≈ {round5(task.duration_min)} min</span>
}

const DAY_MS = 24 * 60 * 60 * 1000

// Days between two dates by the calendar, not by the hours between them.
const daysUntil = (due: Date, now: Date) =>
  Math.round(
    (new Date(due.getFullYear(), due.getMonth(), due.getDate()).getTime() -
      new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime()) /
      DAY_MS,
  )

function dueLabel(due_at: string, now: Date): string | null {
  const due = new Date(due_at)
  if (Number.isNaN(due.getTime())) return null
  if (due.getTime() < now.getTime()) return 'overdue'
  const days = daysUntil(due, now)
  if (days === 0) return 'due today'
  if (days === 1) return 'due tomorrow'
  if (days < 7) return `due ${due.toLocaleDateString(undefined, { weekday: 'short' })}`
  return `due ${due.toLocaleDateString(undefined, { day: 'numeric', month: 'short' })}`
}

function Due({ task }: { task: Task }) {
  if (task.due_at === null) return null
  const label = dueLabel(task.due_at, new Date())
  if (label === null) return null
  return <span className={`task-dur task-due${label === 'overdue' ? ' overdue' : ''}`}>{label}</span>
}

function Row({ node, group, actions }: { node: TaskNode; group: Group; actions: RowActions }) {
  const done = group === 'done'
  const steps = done ? [] : node.children
  const items: OverflowItem[] = [
    group === 'now'
      ? { label: 'Move to Later', run: () => actions.moveToLater(node) }
      : { label: 'Move to Now', run: () => actions.moveToNow(node) },
  ]
  if (steps.length > 0) items.push({ label: 'Keep as one task', run: () => actions.keepAsOne(node) })
  items.push({ label: 'Drop', run: () => actions.drop(node) })
  const sub =
    steps.length > 0
      ? parentSub(node)
      : group === 'now' && node.duration_min === null && node.duration_source === 'none'
        ? 'Note will estimate'
        : null

  return (
    <>
      <div className="task-row">
        <button
          className="tick"
          role="checkbox"
          aria-checked={done}
          aria-label={done ? `Mark ${node.title} not done` : `Mark ${node.title} done`}
          onClick={() => (done ? actions.reopen(node) : actions.complete(node))}
        />
        <div className="task-body">
          <span className="task-title">{node.title}</span>
          {sub && <span className="task-sub">{sub}</span>}
        </div>
        {!done && <Duration task={node} />}
        {!done && <Due task={node} />}
        {!done && (
          <Overflow className="task-more" label={`More actions for ${node.title}`} items={items} />
        )}
        {group === 'now' && (
          <button
            className="task-start"
            aria-label={`Start ${focusTarget(node).title}`}
            onClick={() => actions.startFocus(node)}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M9 7.5v9l7-4.5z" />
            </svg>
          </button>
        )}
      </div>
      {steps.length > 0 && (
        <ul className="task-steps">
          {steps.map((c) => (
            <li key={c.id} className={`task-step${c.state === 'done' ? ' done' : ''}`}>
              <button
                className="tick"
                role="checkbox"
                aria-checked={c.state === 'done'}
                aria-label={
                  c.state === 'done' ? `Mark ${c.title} not done` : `Mark ${c.title} done`
                }
                onClick={() =>
                  c.state === 'done' ? actions.reopenStep(c) : actions.complete(node, c)
                }
              />
              <span className="task-step-title">{c.title}</span>
              {c.duration_min !== null && (
                <span className="task-step-min">{round5(c.duration_min)}</span>
              )}
            </li>
          ))}
        </ul>
      )}
    </>
  )
}
