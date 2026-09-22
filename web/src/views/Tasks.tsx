import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type FormEvent,
  type RefObject,
} from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { collapse, flip, settle } from '../motion-gsap'
import { reducedMotion } from '../motion'
import { Overflow, type OverflowItem } from '../overflow'
import { Tick } from '../tick'
import '../styles/tasks.css'
import type { NewStep, Task, TaskNode, TaskNotify, TaskState, TaskUpdate } from '../types'

const NOW_CAP = 3
const UNDO_MS = 5000
const NOW_FULL = 'Now is full — finish or move something first'

// What a block laid for the task does when it starts.
const ANNOUNCE: { id: TaskNotify; label: string }[] = [
  { id: 'none', label: 'None' },
  { id: 'chat', label: 'Chat' },
  { id: 'notify', label: 'Notify' },
]

type Group = 'now' | 'later' | 'done'

// The prior states a completion has to put back, steps first: reopening a step
// cascades the parent, so the parent's own state must land last to win.
type Snapshot = { id: number; state: TaskState; progress: number }[]

const isLive = (t: Task) => t.state === 'open' || t.state === 'in_progress'

// The server only stores multiples of five; rounding here means no display path
// can ever show a duration the user could not have been offered.
const round5 = (min: number) => Math.max(5, Math.round(min / 5) * 5)

// A parent hands its focus session off to its next unfinished step.
const focusTarget = (node: TaskNode): Task =>
  node.children.find((c) => c.state !== 'done') ?? node

const shown = (t: Task) => (t.state === 'done' ? 100 : t.progress)

// The weighting the server holds a parent to: each live step by its length, one
// without a length by its siblings' mean, a finished one counted full.
function stepsProgress(steps: Task[]): number | null {
  const live = steps.filter((s) => s.state !== 'dropped')
  if (live.length === 0) return null
  const sized = live.flatMap((s) => (s.duration_min === null ? [] : [s.duration_min]))
  const mean = sized.length > 0 ? sized.reduce((a, b) => a + b, 0) / sized.length : 1
  let sum = 0
  let weight = 0
  for (const s of live) {
    const w = s.duration_min ?? mean
    sum += w * shown(s)
    weight += w
  }
  return Math.round(sum / weight)
}

const nodeProgress = (node: TaskNode) =>
  node.state === 'done' ? 100 : (stepsProgress(node.children) ?? node.progress)

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

function withState(
  nodes: TaskNode[],
  id: number,
  state: TaskState,
  progress?: number,
): TaskNode[] {
  const stamp = new Date().toISOString()
  const put = <T extends Task>(t: T): T => ({
    ...t,
    state,
    updated_at: stamp,
    progress: state === 'done' ? 100 : (progress ?? t.progress),
  })
  return nodes.map((n) => {
    if (n.id === id) return put(n)
    if (!n.children.some((c) => c.id === id)) return n
    return { ...n, children: n.children.map((c) => (c.id === id ? put(c) : c)) }
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

// A finished or dropped task keeps its place in the list until it has folded away.
type Leaving = { id: number; group: Exclude<Group, 'done'>; index: number; state: TaskState }

type Placed = { node: TaskNode; leaving: TaskState | null }

function placed(list: TaskNode[], nodes: TaskNode[], leaving: Leaving[], group: Group): Placed[] {
  const out: Placed[] = list.map((node) => ({ node, leaving: null }))
  for (const l of leaving) {
    if (l.group !== group) continue
    const node = nodes.find((n) => n.id === l.id)
    if (node) out.splice(Math.min(l.index, out.length), 0, { node, leaving: l.state })
  }
  return out
}

// Rows that were already there slide from where they were; rows that are new
// settle in. A row folding away drives the layout itself, so both stand down
// for it — and for the frame in which it leaves the list.
function useRowMotion(root: RefObject<HTMLDivElement | null>, busy: boolean) {
  const tops = useRef<Map<string, DOMRect> | null>(null)
  const idle = useRef(true)

  useLayoutEffect(() => {
    const now = new Map<string, DOMRect>()
    const els = new Map<string, HTMLElement>()
    root.current?.querySelectorAll<HTMLElement>('[data-row]').forEach((el) => {
      const key = el.dataset.row as string
      now.set(key, el.getBoundingClientRect())
      els.set(key, el)
    })
    const was = tops.current
    tops.current = now
    if (busy || !idle.current) {
      idle.current = !busy
      return
    }
    if (was === null) return settle([...els.values()])
    const moves: { el: Element; dx: number; dy: number }[] = []
    const fresh: Element[] = []
    for (const [key, el] of els) {
      const before = was.get(key)
      const at = now.get(key) as DOMRect
      if (before === undefined) fresh.push(el)
      else moves.push({ el, dx: before.left - at.left, dy: before.top - at.top })
    }
    flip(moves)
    settle(fresh)
  })
}

// Dragging the bar fires all the way along; the write waits for the hand to settle.
const PROGRESS_SETTLE_MS = 400

type RowActions = {
  setProgress: (task: Task, progress: number) => void
  complete: (node: TaskNode, step?: Task) => void
  reopen: (node: TaskNode) => void
  reopenStep: (step: Task) => void
  moveToNow: (node: TaskNode) => void
  moveToLater: (node: TaskNode) => void
  drop: (node: TaskNode) => void
  keepAsOne: (node: TaskNode) => void
  startFocus: (node: TaskNode) => void
  announce: (node: TaskNode, notify: TaskNotify) => void
}

export function Tasks({ notify, refresh, openNow }: ViewProps) {
  const [nodes, setNodes] = useState<TaskNode[] | null>(null)
  const [failed, setFailed] = useState(false)
  const [title, setTitle] = useState('')
  const [showDone, setShowDone] = useState(false)
  const [leaving, setLeaving] = useState<Leaving[]>([])
  const unfinished = useRef(new Map<number, number>())
  const root = useRef<HTMLDivElement>(null)
  useRowMotion(root, leaving.length > 0)

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
    async (
      id: number,
      body: { state?: TaskState; is_now?: boolean; notify?: TaskNotify; progress?: number },
    ) => {
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

  // A restored row still folding away stays where it is and unfolds its finish.
  const restore = useCallback(
    async (snap: Snapshot) => {
      setLeaving((ls) => ls.filter((l) => !snap.some((s) => s.id === l.id)))
      setNodes((ns) =>
        ns ? snap.reduce((acc, s) => withState(acc, s.id, s.state, s.progress), ns) : ns,
      )
      for (const s of snap) await patch(s.id, { state: s.state, progress: s.progress })
    },
    [patch],
  )

  const markLeaving = (node: TaskNode, state: TaskState) => {
    if (!nodes) return
    const g = groups(nodes)
    const now = g.now.findIndex((n) => n.id === node.id)
    const index = now === -1 ? g.later.findIndex((n) => n.id === node.id) : now
    if (index === -1) return
    setLeaving((ls) => [...ls, { id: node.id, group: now === -1 ? 'later' : 'now', index, state }])
  }
  const gone = useCallback((id: number) => setLeaving((ls) => ls.filter((l) => l.id !== id)), [])

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
    const snap: Snapshot = ordered.map((t) => ({ id: t.id, state: t.state, progress: t.progress }))
    for (const s of snap) unfinished.current.set(s.id, s.progress)
    if (!step || lastStep) markLeaving(node, 'done')
    setNodes((ns) => (ns ? snap.reduce((acc, s) => withState(acc, s.id, 'done'), ns) : ns))
    void patch(step ? step.id : node.id, { state: 'done' })
    notify(`${step && !lastStep ? step.title : node.title} — done`, {
      label: 'Undo',
      run: () => void restore(snap),
      windowMs: UNDO_MS,
    })
  }

  // Reopening a finished task brings its steps back too, so the sub-line's count
  // cannot claim work is done under a task that is open again. Each goes back to
  // the progress it had before it was finished here, when that is known.
  const reopened = (t: Task) => ({
    id: t.id,
    state: 'open' as TaskState,
    progress: unfinished.current.get(t.id) ?? t.progress,
  })
  const reopen = (node: TaskNode) => void restore([...node.children.map(reopened), reopened(node)])

  const reopenStep = (step: Task) => void restore([reopened(step)])

  // Dropping a task drops every step it still has, finished ones included, and a
  // dropped step never comes back on its own, so undo has to name each of them.
  const drop = (node: TaskNode) => {
    const steps = node.children.filter((c) => c.state !== 'dropped')
    const snap: Snapshot = [...steps, node].map((t) => ({
      id: t.id,
      state: t.state,
      progress: t.progress,
    }))
    markLeaving(node, 'dropped')
    setNodes((ns) => (ns ? snap.reduce((acc, s) => withState(acc, s.id, 'dropped'), ns) : ns))
    void patch(node.id, { state: 'dropped' })
    notify(`${node.title} — dropped`, {
      label: 'Undo',
      run: () => void restore(snap),
      windowMs: UNDO_MS,
    })
  }

  const setProgress = (task: Task, progress: number) => {
    setNodes((ns) => (ns ? withTask(ns, { ...task, progress }) : ns))
    void patch(task.id, { progress })
  }

  const announce = (node: TaskNode, notify: TaskNotify) => {
    setNodes((ns) => (ns ? ns.map((n) => (n.id === node.id ? { ...n, notify } : n)) : ns))
    void patch(node.id, { notify })
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
      title: node.title,
      task_id: node.id,
      notes: target.notes,
      ...(index !== -1 && {
        step_index: index + 1,
        step_count: node.children.length,
        step_name: target.title,
      }),
      ...(target.duration_min !== null && { planned_min: round5(target.duration_min) }),
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
      notify: 'notify',
      progress: 0,
      expected_min: null,
      remaining_min: null,
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
    setProgress,
    complete,
    reopen,
    reopenStep,
    moveToNow: (node) => (g.now.length >= NOW_CAP ? notify(NOW_FULL) : setNow(node, true)),
    moveToLater: (node) => setNow(node, false),
    drop,
    keepAsOne,
    startFocus,
    announce,
  }

  const now = placed(g.now, nodes, leaving, 'now')
  const later = placed(g.later, nodes, leaving, 'later')

  return (
    <div className="tasks" ref={root}>
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
      {now.length > 0 && (
        <section className="task-group now">
          <h3 className="task-group-head">NOW</h3>
          <div className="task-list">
            {now.map((p) => (
              <Row key={p.node.id} node={p.node} group="now" actions={actions} leaving={p.leaving} onGone={gone} />
            ))}
          </div>
        </section>
      )}
      {later.length > 0 && (
        <section className="task-group later">
          <h3 className="task-group-head">LATER · {g.later.length}</h3>
          <div className="task-list">
            {later.map((p) => (
              <Row key={p.node.id} node={p.node} group="later" actions={actions} leaving={p.leaving} onGone={gone} />
            ))}
          </div>
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
          {showDone && (
            <div className="task-list">
              {g.doneToday.map((n) => (
                <Row key={n.id} node={n} group="done" actions={actions} leaving={null} onGone={gone} />
              ))}
            </div>
          )}
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

const PROGRESS_STEP = 5

const snapProgress = (v: number) =>
  Math.min(100, Math.max(0, Math.round(v / PROGRESS_STEP) * PROGRESS_STEP))

const PROGRESS_KEYS: Record<string, (v: number) => number> = {
  ArrowRight: (v) => v + PROGRESS_STEP,
  ArrowUp: (v) => v + PROGRESS_STEP,
  ArrowLeft: (v) => v - PROGRESS_STEP,
  ArrowDown: (v) => v - PROGRESS_STEP,
  PageUp: (v) => v + 25,
  PageDown: (v) => v - 25,
  Home: () => 0,
  End: () => 100,
}

// The bar is the slider: a press lands the fill under the pointer and a drag
// carries it along. Without `onSet` it only reads.
function Progress({
  task,
  value: held,
  onSet,
}: {
  task: Task
  value: number
  onSet?: (progress: number) => void
}) {
  const [value, setValue] = useState(held)
  const [dragging, setDragging] = useState(false)
  const track = useRef<HTMLSpanElement>(null)
  const latest = useRef(held)
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => {
    if (dragging) return
    latest.current = held
    setValue(held)
  }, [held, dragging])
  useEffect(() => () => void (timer.current && clearTimeout(timer.current)), [])

  const show = (next: number) => {
    latest.current = next
    setValue(next)
  }
  const commit = (next: number) => {
    if (timer.current) clearTimeout(timer.current)
    if (next !== held) onSet?.(next)
  }
  const at = (clientX: number) => {
    const r = (track.current as HTMLSpanElement).getBoundingClientRect()
    return snapProgress(((clientX - r.left) / r.width) * 100)
  }

  const bar = (
    <span className="task-bar">
      <span className="task-bar-fill" style={{ width: `${value}%` }} />
    </span>
  )
  const left = task.remaining_min !== null && (
    <span className="task-left">{task.remaining_min} min left</span>
  )

  if (!onSet)
    return (
      <span className="task-prog">
        <span
          className="task-bar-wrap"
          role="progressbar"
          aria-valuenow={value}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuetext={`${value}% done`}
          aria-label={`${task.title} progress`}
        >
          {bar}
        </span>
        {left}
      </span>
    )

  return (
    <span className="task-prog">
      <span
        ref={track}
        className="task-bar-wrap"
        role="slider"
        tabIndex={0}
        data-dragging={dragging || undefined}
        aria-valuenow={value}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuetext={`${value}% done`}
        aria-label={`${task.title} progress`}
        onPointerDown={(e) => {
          if (e.button !== 0) return
          e.currentTarget.setPointerCapture(e.pointerId)
          setDragging(true)
          show(at(e.clientX))
        }}
        onPointerMove={(e) => dragging && show(at(e.clientX))}
        onPointerUp={() => {
          if (!dragging) return
          setDragging(false)
          commit(latest.current)
        }}
        onPointerCancel={() => {
          setDragging(false)
          show(held)
        }}
        onKeyDown={(e) => {
          const step = PROGRESS_KEYS[e.key]
          if (!step) return
          e.preventDefault()
          const next = snapProgress(step(latest.current))
          show(next)
          if (timer.current) clearTimeout(timer.current)
          timer.current = setTimeout(() => commit(next), PROGRESS_SETTLE_MS)
        }}
      >
        {bar}
        <span className="task-thumb" style={{ left: `${value}%` }} />
      </span>
      {left}
    </span>
  )
}

// Finishing a row plays out in the stylesheet — the bar fills, the row greys,
// the bar fades — and the row folds away once that has been seen.
const FINISH_HOLD_S = 1.15
const REVIVE_MS = 420

function Row({
  node,
  group,
  actions,
  leaving,
  onGone,
}: {
  node: TaskNode
  group: Group
  actions: RowActions
  leaving: TaskState | null
  onGone: (id: number) => void
}) {
  const item = useRef<HTMLDivElement>(null)
  const [reviving, setReviving] = useState(false)
  const done = group === 'done'
  const finished = (done && !reviving) || leaving === 'done'
  const steps = done ? [] : node.children

  useLayoutEffect(() => {
    if (leaving === null) return
    return collapse(item.current, () => onGone(node.id), leaving === 'done' ? FINISH_HOLD_S : 0)
  }, [leaving, node.id, onGone])

  const revive = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => () => void (revive.current && clearTimeout(revive.current)), [])

  // A finished row is brought back the way it went: it colours in and its bar
  // returns before it moves back to its list.
  const reopen = () => {
    if (reviving) return
    setReviving(true)
    revive.current = setTimeout(() => actions.reopen(node), reducedMotion() ? 0 : REVIVE_MS)
  }

  const items: OverflowItem[] = [
    group === 'now'
      ? { label: 'Move to Later', run: () => actions.moveToLater(node) }
      : { label: 'Move to Now', run: () => actions.moveToNow(node) },
  ]
  if (steps.length > 0) items.push({ label: 'Keep as one task', run: () => actions.keepAsOne(node) })
  for (const choice of ANNOUNCE) {
    items.push({
      label: `Announce: ${choice.label}`,
      run: () => actions.announce(node, choice.id),
      checked: node.notify === choice.id,
    })
  }
  items.push({ label: 'Drop', run: () => actions.drop(node) })
  const sub =
    steps.length > 0
      ? parentSub(node)
      : group === 'now' && node.duration_min === null && node.duration_source === 'none'
        ? 'Note will estimate'
        : null
  const live = !done && leaving === null

  return (
    <div
      className="task-item"
      ref={item}
      data-row={`${leaving ? 'x' : 't'}${node.id}`}
      data-leaving={leaving ?? undefined}
      data-finished={finished || undefined}
    >
      <div className="task-row">
        <Tick
          checked={finished}
          label={done ? `Mark ${node.title} not done` : `Mark ${node.title} done`}
          onClick={() => (done ? reopen() : actions.complete(node))}
        />
        <div className="task-body">
          <span className="task-title">{node.title}</span>
          {sub && <span className="task-sub">{sub}</span>}
        </div>
        <div className="task-foot">
          {!done && (
            <span className="task-chips">
              <Duration task={node} />
              <Due task={node} />
            </span>
          )}
          <Progress
            task={node}
            value={finished ? 100 : nodeProgress(node)}
            onSet={live && steps.length === 0 ? (p) => actions.setProgress(node, p) : undefined}
          />
        </div>
        <span className="task-acts">
          {!done && (
            <Overflow className="task-more" label={`More actions for ${node.title}`} items={items} />
          )}
          {group === 'now' && (
            <button
              className="task-start"
              data-tip="Start"
              aria-label={`Start ${focusTarget(node).title}`}
              onClick={() => actions.startFocus(node)}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M9 7.5v9l7-4.5z" />
              </svg>
            </button>
          )}
        </span>
      </div>
      {steps.length > 0 && (
        <ul className="task-steps">
          {steps.map((c) => (
            <li key={c.id} className={`task-step${c.state === 'done' ? ' done' : ''}`}>
              <Tick
                checked={c.state === 'done'}
                label={c.state === 'done' ? `Mark ${c.title} not done` : `Mark ${c.title} done`}
                onClick={() =>
                  c.state === 'done' ? actions.reopenStep(c) : actions.complete(node, c)
                }
              />
              <span className="task-step-title">{c.title}</span>
              {c.duration_min !== null && (
                <span className="task-step-min">{round5(c.duration_min)}</span>
              )}
              <Progress
                task={c}
                value={shown(c)}
                onSet={live && c.state !== 'done' ? (p) => actions.setProgress(c, p) : undefined}
              />
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
