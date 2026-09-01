import { useCallback, useEffect, useState, type FormEvent } from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { Overflow, type OverflowItem } from '../overflow'
import { SectionTitle } from '../section'
import type { NewStep, Task, TaskNode, TaskState, TaskUpdate } from '../types'

const NOW_CAP = 3
const UNDO_MS = 5000
const NOW_FULL = 'Now is full — finish or move something first'
const NOW_EMPTY = "Nothing queued — pull something up from Later, or just add what's on your mind."

type Group = 'now' | 'later' | 'done'

// The prior states a completion has to put back, steps first: reopening a step
// cascades the parent, so the parent's own state must land last to win.
type Snapshot = { id: number; state: TaskState }[]

const isLive = (t: Task) => t.state === 'open' || t.state === 'in_progress'

// The server only stores multiples of five; rounding here means no display path
// can ever show a duration the user could not have been offered.
const round5 = (min: number) => Math.max(5, Math.round(min / 5) * 5)

// Step 6's Now screen opens on this task; a parent hands off to its next
// unfinished step.
const focusTarget = (node: TaskNode): Task =>
  node.children.find((c) => c.state !== 'done') ?? node

function parentSub(node: TaskNode): string {
  const done = node.children.filter((c) => c.state === 'done').length
  const parts = node.duration_min === null ? [] : [`≈ ${round5(node.duration_min)} min`]
  parts.push(`Note split this into ${node.children.length} steps`, `${done} done`)
  return parts.join(' · ')
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
  setProgress: (node: TaskNode, on: boolean) => void
  keepAsOne: (node: TaskNode) => void
  startFocus: (node: TaskNode) => void
}

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

  const setNow = (node: TaskNode, is_now: boolean) => {
    setNodes((ns) => (ns ? ns.map((n) => (n.id === node.id ? { ...n, is_now } : n)) : ns))
    void patch(node.id, { is_now })
  }

  const setProgress = (node: TaskNode, on: boolean) => {
    const state: TaskState = on ? 'in_progress' : 'open'
    setNodes((ns) => (ns ? withState(ns, node.id, state) : ns))
    void patch(node.id, { state })
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

  // Until the Now screen lands, starting a task does the one thing that screen
  // would change here.
  const startFocus = (node: TaskNode) => {
    if (node.state !== 'in_progress') setProgress(node, true)
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
      children: [],
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

  if (failed)
    return (
      <div className="page tasks">
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
  const empty = g.now.length === 0 && g.later.length === 0 && g.doneToday.length === 0
  const actions: RowActions = {
    complete,
    reopen,
    reopenStep,
    moveToNow: (node) => (g.now.length >= NOW_CAP ? notify(NOW_FULL) : setNow(node, true)),
    moveToLater: (node) => setNow(node, false),
    setProgress,
    keepAsOne,
    startFocus,
  }

  return (
    <div className="page tasks">
      <SectionTitle>Tasks</SectionTitle>
      <form className="task-add" onSubmit={add}>
        <span className="task-add-glyph" aria-hidden="true">
          ＋
        </span>
        <input
          value={title}
          placeholder="Add a task — just a title is enough"
          aria-label="Add a task — just a title is enough"
          onChange={(e) => setTitle(e.target.value)}
        />
        <span className="task-add-hint">press Enter to save</span>
      </form>
      {empty ? (
        <p className="task-empty">{NOW_EMPTY}</p>
      ) : (
        <>
          <section className="task-group now">
            <h3 className="task-group-head">
              NOW{' '}
              <span className="task-group-count">
                · {g.now.length} of {NOW_CAP}
              </span>
              <span className="task-group-why">a short list you can actually finish</span>
            </h3>
            {g.now.length === 0 ? (
              <p className="task-empty">{NOW_EMPTY}</p>
            ) : (
              g.now.map((n) => <Row key={n.id} node={n} group="now" actions={actions} />)
            )}
          </section>
          {g.later.length > 0 && (
            <section className="task-group later">
              <h3 className="task-group-head">
                LATER <span className="task-group-count">· {g.later.length}</span>
              </h3>
              {g.later.map((n) => (
                <Row key={n.id} node={n} group="later" actions={actions} />
              ))}
            </section>
          )}
          {g.doneToday.length > 0 && (
            <section className="task-group done">
              <h3 className="task-group-head">
                DONE TODAY <span className="task-group-count">· {g.doneToday.length}</span>
              </h3>
              {g.doneToday.map((n) => (
                <Row key={n.id} node={n} group="done" actions={actions} />
              ))}
            </section>
          )}
        </>
      )}
    </div>
  )
}

function Duration({ task }: { task: Task }) {
  if (task.duration_min !== null) return <span className="task-dur">≈ {round5(task.duration_min)} min</span>
  if (task.duration_source !== 'none') return null
  return <span className="task-dur est">Note will estimate</span>
}

function Row({ node, group, actions }: { node: TaskNode; group: Group; actions: RowActions }) {
  const done = group === 'done'
  const steps = done ? [] : node.children
  const next = steps.find((c) => c.state !== 'done')
  const items: OverflowItem[] = [
    group === 'now'
      ? { label: 'Move to Later', run: () => actions.moveToLater(node) }
      : { label: 'Move to Now', run: () => actions.moveToNow(node) },
    node.state === 'in_progress'
      ? { label: 'Clear in progress', run: () => actions.setProgress(node, false) }
      : { label: 'Mark in progress', run: () => actions.setProgress(node, true) },
  ]
  if (steps.length > 0) {
    items.push({ label: 'Keep as one task', run: () => actions.keepAsOne(node) })
  }

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
          {steps.length > 0 && <div className="task-sub">{parentSub(node)}</div>}
        </div>
        {!done && node.state === 'in_progress' && (
          <span className="task-tag progress">in progress</span>
        )}
        {!done && steps.length === 0 && <Duration task={node} />}
        {group === 'now' && (
          <button
            className="task-start"
            aria-label={`Start ${focusTarget(node).title}`}
            onClick={() => actions.startFocus(node)}
          >
            <span aria-hidden="true">▶</span>
          </button>
        )}
        {!done && (
          <Overflow className="task-more" label={`More actions for ${node.title}`} items={items} />
        )}
      </div>
      {steps.length > 0 && (
        <div className="task-steps">
          {steps.map((c) => (
            <div
              key={c.id}
              className={`task-step${c.state === 'done' ? ' done' : ''}${c.id === next?.id ? ' next' : ''}`}
            >
              <button
                className="task-tick"
                aria-label={
                  c.state === 'done' ? `Mark ${c.title} not done` : `Mark ${c.title} done`
                }
                onClick={() =>
                  c.state === 'done' ? actions.reopenStep(c) : actions.complete(node, c)
                }
              >
                <span className="ticksm" aria-hidden="true" />
              </button>
              <span className="task-step-title">{c.title}</span>
              {c.duration_min !== null && (
                <span className="task-dur">{round5(c.duration_min)} min</span>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
