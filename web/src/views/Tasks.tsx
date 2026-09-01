import { useCallback, useEffect, useState, type FormEvent } from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { Overflow, type OverflowItem } from '../overflow'
import { SectionTitle } from '../section'
import type { Task, TaskNode, TaskState, TaskUpdate } from '../types'

const NOW_CAP = 3
const UNDO_MS = 5000
const NOW_FULL = 'Now is full — finish or move something first'
const NOW_EMPTY = "Nothing queued — pull something up from Later, or just add what's on your mind."

type Group = 'now' | 'later' | 'done'

// The prior states a completion has to put back, steps first: reopening a step
// cascades the parent, so the parent's own state must land last to win.
type Snapshot = { id: number; state: TaskState }[]

const isLive = (t: Task) => t.state === 'open' || t.state === 'in_progress'

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
  complete: (node: TaskNode) => void
  reopen: (node: TaskNode) => void
  moveToNow: (node: TaskNode) => void
  moveToLater: (node: TaskNode) => void
  setProgress: (node: TaskNode, on: boolean) => void
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

  // Finishing a task takes its live steps with it, so undo has to carry them
  // back or the steps would stay ticked under a reopened task.
  const complete = (node: TaskNode) => {
    const steps = node.children.filter((c) => c.state !== 'done')
    const snap: Snapshot = [
      ...steps.map((c) => ({ id: c.id, state: c.state })),
      { id: node.id, state: node.state },
    ]
    setNodes((ns) => {
      if (!ns) return ns
      return snap.reduce((acc, s) => withState(acc, s.id, 'done'), ns)
    })
    void patch(node.id, { state: 'done' })
    notify(`${node.title} — done`, {
      label: 'Undo',
      run: () => void restore(snap),
      windowMs: UNDO_MS,
    })
  }

  const reopen = (node: TaskNode) =>
    void restore([
      ...node.children.map((c) => ({ id: c.id, state: 'open' as TaskState })),
      { id: node.id, state: 'open' as TaskState },
    ])

  const setNow = (node: TaskNode, is_now: boolean) => {
    setNodes((ns) => (ns ? ns.map((n) => (n.id === node.id ? { ...n, is_now } : n)) : ns))
    void patch(node.id, { is_now })
  }

  const setProgress = (node: TaskNode, on: boolean) => {
    const state: TaskState = on ? 'in_progress' : 'open'
    setNodes((ns) => (ns ? withState(ns, node.id, state) : ns))
    void patch(node.id, { state })
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
    moveToNow: (node) =>
      g.now.length >= NOW_CAP ? notify(NOW_FULL) : setNow(node, true),
    moveToLater: (node) => setNow(node, false),
    setProgress,
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

function Row({
  node,
  group,
  actions,
}: {
  node: TaskNode
  group: Group
  actions: RowActions
}) {
  const done = group === 'done'
  const items: OverflowItem[] = [
    group === 'now'
      ? { label: 'Move to Later', run: () => actions.moveToLater(node) }
      : { label: 'Move to Now', run: () => actions.moveToNow(node) },
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
        {!done && node.state === 'in_progress' && (
          <span className="task-tag progress">in progress</span>
        )}
        {!done && (
          <Overflow
            className="task-more"
            label={`More actions for ${node.title}`}
            items={items}
          />
        )}
      </div>
    </div>
  )
}
