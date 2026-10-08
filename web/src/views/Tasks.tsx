import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
  type RefObject,
} from 'react'
import { api, ApiError } from '../api'
import { latest } from '../coalesce'
import { dropped, useLiftDrag, withOrder, type DropTo } from '../order'
import type { ViewProps } from '../app'
import { collapse, flip, settle } from '../motion-gsap'
import { reducedMotion } from '../motion'
import { Overflow, useMenuSheet, type OverflowItem } from '../overflow'
import { Tick } from '../tick'
import '../styles/tasks.css'
import type { Goal, NewStep, Task, TaskNode, TaskNotify, TaskState, TaskUpdate, TaskUrgency } from '../types'
import { t, type Key } from '../i18n'
import * as format from '../i18n/format'

const NOW_CAP = 3
const UNDO_MS = 5000

// What a block laid for the task does when it starts.
const ANNOUNCE: { id: TaskNotify; label: Key }[] = [
  { id: 'none', label: 'announce.none' },
  { id: 'chat', label: 'announce.chat' },
  { id: 'notify', label: 'announce.notify' },
]

type Group = 'soon' | 'later' | 'done'

const SORTS = [
  { id: 'schedule', label: 'tasks.sort.schedule' },
  { id: 'due', label: 'tasks.sort.due' },
  { id: 'urgency', label: 'tasks.sort.urgency' },
  { id: 'newest', label: 'tasks.sort.newest' },
  { id: 'category', label: 'tasks.sort.category' },
] as const

type SortKey = (typeof SORTS)[number]['id']

const SORT_KEY = 'note.taskSort'

function storedSort(): SortKey {
  try {
    const held = localStorage.getItem(SORT_KEY)
    const found = SORTS.find((s) => s.id === held)
    if (found) return found.id
  } catch {
    // storage blocked; the default order holds
  }
  return 'schedule'
}

function keepSort(id: SortKey) {
  try {
    localStorage.setItem(SORT_KEY, id)
  } catch {
    // storage blocked; the choice still holds for this session
  }
}

// An undated or unscheduled task sorts behind every dated one.
function when(iso: string | null): number {
  if (iso === null) return Infinity
  const t = new Date(iso).getTime()
  return Number.isNaN(t) ? Infinity : t
}

const earlier = (a: string | null, b: string | null) => {
  const [x, y] = [when(a), when(b)]
  return x === y ? 0 : x < y ? -1 : 1
}

const newest = (a: TaskNode, b: TaskNode) => b.id - a.id

const bySchedule = (a: TaskNode, b: TaskNode) =>
  earlier(a.scheduled_at, b.scheduled_at) || earlier(a.due_at, b.due_at) || newest(a, b)

const byUrgency = (a: TaskNode, b: TaskNode) =>
  urgencyRank(a) - urgencyRank(b) || earlier(a.due_at, b.due_at) || newest(a, b)

const COMPARE: Record<SortKey, (a: TaskNode, b: TaskNode) => number> = {
  schedule: (a, b) => urgencyRank(a) - urgencyRank(b) || bySchedule(a, b),
  due: (a, b) => earlier(a.due_at, b.due_at) || newest(a, b),
  urgency: byUrgency,
  newest,
  category: bySchedule,
}

// Every word has to land somewhere on the task for it to stay in the list.
function matches(node: TaskNode, words: string[]): boolean {
  if (words.length === 0) return true
  const hay = [node.title, node.description, node.notes, node.category, node.goal_title ?? '']
    .join(' ')
    .toLowerCase()
  return words.every((w) => hay.includes(w))
}

// The category rides after the title as `.meta`, so a title that opens with it
// says it twice. Display only: the stored title, and the search, keep the prefix.
function shownTitle(node: TaskNode): string {
  const prefix = `${node.category} — `
  return node.category !== '' && node.title.startsWith(prefix)
    ? node.title.slice(prefix.length)
    : node.title
}

// The user's categories, the ones they use most first.
function categoriesOf(nodes: TaskNode[]): string[] {
  const counts = new Map<string, number>()
  for (const n of nodes) {
    if (!isLive(n) || n.category === '') continue
    counts.set(n.category, (counts.get(n.category) ?? 0) + 1)
  }
  return [...counts.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([c]) => c)
}

// Later's rows under their category headings, in the pills' order, the
// uncategorised ones last.
function byCategory(list: Placed[], categories: string[]): [string, Placed[]][] {
  const out: [string, Placed[]][] = []
  for (const category of [...categories, '']) {
    const rows = list.filter((p) => p.node.category === category)
    if (rows.length > 0) out.push([category === '' ? t('tasks.uncategorised') : category, rows])
  }
  return out
}

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
  return t('tasks.stepsDone', { done, count: node.children.length })
}

function sameDay(iso: string, today: Date): boolean {
  const d = new Date(iso)
  return (
    d.getFullYear() === today.getFullYear() &&
    d.getMonth() === today.getMonth() &&
    d.getDate() === today.getDate()
  )
}

const SOON_DAYS = 7

const near = (iso: string | null, today: Date) =>
  iso !== null && daysUntil(new Date(iso), today) < SOON_DAYS

// The server caps Now itself, but a frame between an agent write and the reload
// must not count a fourth: the newest live members fall out of it, which is the
// order the server demotes in. Soon is Now, the urgent, and whatever falls due or
// is planned within the week; the rest waits folded away.
function groups(nodes: TaskNode[]) {
  const today = new Date()
  const live = nodes.filter(isLive)
  const now = live.filter((n) => n.is_now).slice(0, NOW_CAP)
  const inNow = new Set(now.map((n) => n.id))
  const soon = (n: TaskNode) =>
    inNow.has(n.id) ||
    n.urgency === 'high' ||
    n.pressing ||
    near(n.due_at, today) ||
    near(n.scheduled_at, today)
  return {
    now,
    soon: live.filter(soon),
    later: live.filter((n) => !soon(n)),
    doneToday: nodes.filter((n) => n.state === 'done' && sameDay(n.updated_at, today)),
  }
}

const searchWords = (title: string) => {
  const search = title.trim().toLowerCase()
  return search === '' ? [] : search.split(/\s+/)
}

const shownBy = (filter: string | null, words: string[]) => (n: TaskNode) =>
  (filter === null || n.category === filter) && matches(n, words)

// The live rows in the order they are drawn, before any leaving row is put back:
// today's order heads Soon, pulling in any ordered task Later would hold.
function shownGroups(
  nodes: TaskNode[],
  filter: string | null,
  title: string,
  sort: SortKey,
  order: number[],
) {
  const visible = shownBy(filter, searchWords(title))
  const g = groups(nodes)
  const sorted = COMPARE[sort]
  const lifted = withOrder(
    g.soon.filter(visible).sort(sorted),
    nodes.filter((n) => isLive(n) && visible(n)),
    order,
  )
  return {
    soon: lifted.rows,
    ordered: lifted.ordered,
    later: g.later.filter((n) => visible(n) && !lifted.rows.includes(n)).sort(sorted),
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

// Rows that were already there slide from where they were; rows that are new settle
// in. While a row folds away it drives the layout itself, so the FLIP stands down and
// `mark` takes the snapshot the next pass compares against.
function useRowMotion(root: RefObject<HTMLDivElement | null>, busy: boolean) {
  const tops = useRef<Map<string, DOMRect> | null>(null)

  const read = useCallback(() => {
    const at = new Map<string, DOMRect>()
    const els = new Map<string, HTMLElement>()
    root.current?.querySelectorAll<HTMLElement>('[data-row]').forEach((el) => {
      const key = el.dataset.row as string
      at.set(key, el.getBoundingClientRect())
      els.set(key, el)
    })
    return { at, els }
  }, [root])

  // Called with a folded row at zero height, so rows that already slid up are not
  // moved again by the pass that follows its removal.
  const mark = useCallback(() => {
    tops.current = read().at
  }, [read])

  useLayoutEffect(() => {
    if (busy || root.current?.querySelector('[data-dragging]')) return
    const { at, els } = read()
    const was = tops.current
    tops.current = at
    if (was === null) return settle([...els.values()])
    const moves: { el: Element; dx: number; dy: number }[] = []
    const fresh: Element[] = []
    for (const [key, el] of els) {
      const before = was.get(key)
      const now = at.get(key) as DOMRect
      if (before === undefined) fresh.push(el)
      else moves.push({ el, dx: before.left - now.left, dy: before.top - now.top })
    }
    flip(moves)
    settle(fresh)
  })

  return { mark }
}

// Dragging the bar fires all the way along; the write waits for the hand to settle.
const PROGRESS_SETTLE_MS = 400

type RowActions = {
  setCategory: (node: TaskNode, category: string) => void
  setUrgency: (node: TaskNode, urgency: TaskUrgency) => void
  setGoal: (node: TaskNode, goal_id: number | null) => void
  setProgress: (task: Task, progress: number) => void
  complete: (node: TaskNode, step?: Task) => void
  reopen: (node: TaskNode) => void
  reopenStep: (step: Task) => void
  moveToNow: (node: TaskNode) => void
  moveToLater: (node: TaskNode) => void
  drop: (node: TaskNode) => void
  mergeSteps: (node: TaskNode) => void
  startFocus: (node: TaskNode) => void
  announce: (node: TaskNode, notify: TaskNotify) => void
  toggleSteps: (id: number) => void
}

export function Tasks({ notify, refresh, openNow }: ViewProps) {
  const [nodes, setNodes] = useState<TaskNode[] | null>(null)
  const [goals, setGoals] = useState<Goal[]>([])
  const [failed, setFailed] = useState(false)
  const [title, setTitle] = useState('')
  const [showDone, setShowDone] = useState(false)
  const [showLater, setShowLater] = useState(false)
  const [leaving, setLeaving] = useState<Leaving[]>([])
  const [openSteps, setOpenSteps] = useState<Set<number>>(new Set())
  const [openGoals, setOpenGoals] = useState<Set<number>>(new Set())
  const [filter, setFilter] = useState<string | null>(null)
  const [sort, setSort] = useState<SortKey>(storedSort)
  const [goalDraft, setGoalDraft] = useState<{ title: string; due: string } | null>(null)
  const [runOrder, setRunOrder] = useState<number[]>([])
  const dropRef = useRef<DropTo | null>(null)
  useLiftDrag(dropRef)
  const seeded = useRef(false)
  const unfinished = useRef(new Map<number, number>())
  const root = useRef<HTMLDivElement>(null)
  const { mark } = useRowMotion(root, leaving.length > 0)
  const shown = useMemo(
    () => (nodes ? shownGroups(nodes, filter, title, sort, runOrder) : null),
    [nodes, filter, title, sort, runOrder],
  )

  const loadGoals = useCallback(() => {
    api
      .goals()
      .then((gs) => setGoals(gs.filter((g) => g.state === 'open')))
      .catch(() => setGoals([]))
  }, [])

  const newest = useState(() => latest<TaskNode[]>())[0]
  const load = useCallback(() => {
    loadGoals()
    api
      .order()
      .then((o) => setRunOrder(o.task_ids))
      .catch(() => setRunOrder([]))
    newest(api.tasks())
      .then((ts) => {
        if (!ts) return
        setNodes(ts)
        setFailed(false)
        if (seeded.current) return
        seeded.current = true
        setOpenSteps(new Set(ts.filter((t) => t.is_now && t.children.length > 0).map((t) => t.id)))
      })
      .catch(() => setFailed(true))
  }, [loadGoals, newest])
  useEffect(load, [load, refresh])

  const patch = useCallback(
    async (
      id: number,
      body: {
        state?: TaskState
        is_now?: boolean
        notify?: TaskNotify
        progress?: number
        category?: string
        urgency?: TaskUrgency
        goal_id?: number | null
      },
    ) => {
      try {
        const updated = await api.patchTask(id, body)
        setNodes((ns) => (ns ? mergeUpdate(ns, updated) : ns))
      } catch (err) {
        notify(
          err instanceof ApiError && err.status === 409
            ? t('tasks.nowFull')
            : t('tasks.updateFailed'),
        )
        load()
      }
    },
    [load, notify],
  )

  // A restored row still folding away stays where it is and unfolds its finish.
  const restore = useCallback(
    async (snap: Snapshot) => {
      mark()
      setLeaving((ls) => ls.filter((l) => !snap.some((s) => s.id === l.id)))
      setNodes((ns) =>
        ns ? snap.reduce((acc, s) => withState(acc, s.id, s.state, s.progress), ns) : ns,
      )
      for (const s of snap) await patch(s.id, { state: s.state, progress: s.progress })
      loadGoals()
    },
    [loadGoals, mark, patch],
  )

  const markLeaving = (node: TaskNode, state: TaskState) => {
    if (!shown) return
    const soon = shown.soon.findIndex((n) => n.id === node.id)
    const index = soon === -1 ? shown.later.findIndex((n) => n.id === node.id) : soon
    if (index === -1) return
    setLeaving((ls) => [...ls, { id: node.id, group: soon === -1 ? 'later' : 'soon', index, state }])
  }
  const gone = useCallback(
    (id: number) => {
      mark()
      setLeaving((ls) => ls.filter((l) => l.id !== id))
    },
    [mark],
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
    const snap: Snapshot = ordered.map((t) => ({ id: t.id, state: t.state, progress: t.progress }))
    for (const s of snap) unfinished.current.set(s.id, s.progress)
    if (!step || lastStep) markLeaving(node, 'done')
    setNodes((ns) => (ns ? snap.reduce((acc, s) => withState(acc, s.id, 'done'), ns) : ns))
    void patch(step ? step.id : node.id, { state: 'done' }).then(loadGoals)
    notify(t('tasks.doneToast', { title: step && !lastStep ? step.title : node.title }), {
      label: t('toast.undo'),
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
    void patch(node.id, { state: 'dropped' }).then(loadGoals)
    notify(t('tasks.droppedToast', { title: node.title }), {
      label: t('toast.undo'),
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

  // A category rides on the task and is read back by its steps.
  const setCategory = (node: TaskNode, category: string) => {
    setNodes((ns) =>
      ns
        ? ns.map((n) =>
            n.id === node.id
              ? { ...n, category, children: n.children.map((c) => ({ ...c, category })) }
              : n,
          )
        : ns,
    )
    if (filter !== null && filter !== category) setFilter(null)
    void patch(node.id, { category })
  }

  const setUrgency = (node: TaskNode, urgency: TaskUrgency) => {
    setNodes((ns) =>
      ns
        ? ns.map((n) =>
            n.id === node.id
              ? { ...n, urgency, children: n.children.map((c) => ({ ...c, urgency })) }
              : n,
          )
        : ns,
    )
    void patch(node.id, { urgency })
  }

  const setGoal = (node: TaskNode, goal_id: number | null) => {
    const goal = goals.find((g) => g.id === goal_id) ?? null
    setNodes((ns) =>
      ns
        ? ns.map((n) =>
            n.id === node.id ? { ...n, goal_id, goal_title: goal ? goal.title : null } : n,
          )
        : ns,
    )
    void patch(node.id, { goal_id }).then(loadGoals)
  }

  // A task carries the fold its group asks for: open in Now, closed in Later.
  const setNow = (node: TaskNode, is_now: boolean) => {
    setNodes((ns) => (ns ? ns.map((n) => (n.id === node.id ? { ...n, is_now } : n)) : ns))
    setOpenSteps((open) => {
      const next = new Set(open)
      if (is_now) next.add(node.id)
      else next.delete(node.id)
      return next
    })
    void patch(node.id, { is_now })
  }

  const toggleGoal = (id: number) =>
    setOpenGoals((open) => {
      const next = new Set(open)
      if (!next.delete(id)) next.add(id)
      return next
    })

  const editGoal = (goal: Goal, patch: { title?: string; due_at?: string | null }) => {
    setGoals((gs) => gs.map((g) => (g.id === goal.id ? { ...g, ...patch } : g)))
    api.patchGoal(goal.id, patch).catch(() => {
      notify(t('goals.updateFailed'))
      loadGoals()
    })
  }

  const closeGoal = (goal: Goal, state: 'done' | 'dropped') => {
    setGoals((gs) => gs.filter((g) => g.id !== goal.id))
    api
      .patchGoal(goal.id, { state })
      .then(() => load())
      .catch(() => {
        notify(t('goals.updateFailed'))
        loadGoals()
      })
    notify(t(state === 'done' ? 'tasks.doneToast' : 'tasks.droppedToast', { title: goal.title }), {
      label: t('toast.undo'),
      run: () =>
        void api
          .patchGoal(goal.id, { state: 'open' })
          .then(() => load())
          .catch(() => notify(t('goals.restoreFailed'))),
      windowMs: UNDO_MS,
    })
  }

  const addGoal = (e: FormEvent) => {
    e.preventDefault()
    const draft = goalDraft
    const text = draft?.title.trim()
    if (!draft || !text) return
    setGoalDraft(null)
    api
      .addGoal({ title: text, due_at: draft.due === '' ? null : endOfDay(draft.due) })
      .then((g) => {
        setGoals((gs) => [...gs, g])
        setOpenGoals((open) => new Set(open).add(g.id))
      })
      .catch(() => notify(t('goals.addFailed')))
  }

  const toggleSteps = (id: number) =>
    setOpenSteps((open) => {
      const next = new Set(open)
      if (!next.delete(id)) next.add(id)
      return next
    })

  // The only reversal a flatten has is replaying the split, so the steps travel
  // into the undo closure rather than being read back off a stale row.
  const mergeSteps = (node: TaskNode) => {
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
        notify(t('tasks.updateFailed'))
        load()
      })
    notify(t('tasks.mergedToast', { title: node.title }), {
      label: t('toast.undo'),
      run: () =>
        void api
          .splitTask(node.id, steps)
          .then(put)
          .catch(() => {
            notify(t('tasks.splitFailed'))
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
    const is_now = g.soon.length === 0 && g.later.length === 0
    // A task added while a category is picked lands in it, so the list the user
    // is looking at is the list it joins.
    const category = filter ?? ''
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
      category,
      urgency: 'normal',
      pressing: false,
      goal_id: null,
      goal_title: null,
      scheduled_at: null,
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
      .addTask(text, { ...(is_now && { is_now: true }), ...(category !== '' && { category }) })
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
        notify(t('tasks.addFailed'))
      })
    notify(t('tasks.addedToast', { title: text }), {
      label: t('toast.undo'),
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
          {t('tasks.loadFailed')}{' '}
          <button className="quiet" onClick={load}>
            {t('common.retry')}
          </button>
        </p>
      </div>
    )
  if (nodes === null) return null

  const g = groups(nodes)
  const actions: RowActions = {
    setCategory,
    setUrgency,
    setGoal,
    setProgress,
    complete,
    reopen,
    reopenStep,
    moveToNow: (node) => (g.now.length >= NOW_CAP ? notify(t('tasks.nowFull')) : setNow(node, true)),
    moveToLater: (node) => setNow(node, false),
    drop,
    mergeSteps,
    startFocus,
    announce,
    toggleSteps,
  }

  const search = title.trim().toLowerCase()
  const categories = categoriesOf(nodes)
  const visible = shownBy(filter, searchWords(title))

  const soon = placed(shown?.soon ?? [], nodes, leaving, 'soon')
  const later = placed(shown?.later ?? [], nodes, leaving, 'later')
  // A task settles into Done today only once it has folded away: until then it is
  // still in the list it is leaving, and a row is never in two lists at once.
  const doneToday = g.doneToday.filter((n) => !leaving.some((l) => l.id === n.id))
  const nothingFound = search !== '' && soon.length === 0 && later.length === 0
  const laterOpen = showLater || search !== ''
  const orderedIds = new Set((shown?.soon ?? []).slice(0, shown?.ordered ?? 0).map((n) => n.id))
  const edgeAt = soon.findIndex((p) => !orderedIds.has(p.node.id))
  const head = edgeAt === -1 ? soon : soon.slice(0, edgeAt)
  const rest = edgeAt === -1 ? [] : soon.slice(edgeAt)
  const canDrag = filter === null && search === '' && leaving.length === 0
  const dragRows = [
    ...head,
    ...(sort === 'category' ? byCategory(rest, categories).flatMap(([, rows]) => rows) : rest),
  ].map((p) => p.node)
  dropRef.current = canDrag
    ? ({ from, to, into }, commit) => {
        const next = dropped(runOrder, dragRows, head.length, from, to, into)
        const same = next.length === runOrder.length && next.every((id, i) => id === runOrder[i])
        if (same) return false
        if (!commit) return true
        mark()
        setRunOrder(next)
        api
          .setOrder(next)
          .then((o) => setRunOrder(o.task_ids))
          .catch(() => {
            notify(t('tasks.updateFailed'))
            load()
          })
        return true
      }
    : null

  const row = (p: Placed, group: Exclude<Group, 'done'>, scope = '', drag = false) => (
    <Row
      key={p.node.id}
      node={p.node}
      group={group}
      scope={scope}
      actions={actions}
      leaving={p.leaving}
      onGone={gone}
      stepsOpen={openSteps.has(p.node.id)}
      categories={categories}
      goals={goals}
      drag={drag}
    />
  )

  const list = (ps: Placed[], group: Exclude<Group, 'done'>, drag = false) =>
    sort === 'category' ? (
      byCategory(ps, categories).map(([head, rows]) => (
        <div key={head} className="task-cat">
          <h4 className="task-sub-head">{head}</h4>
          <div className="task-list">{rows.map((p) => row(p, group, '', drag))}</div>
        </div>
      ))
    ) : (
      <div className="task-list">{ps.map((p) => row(p, group, '', drag))}</div>
    )

  return (
    <div className="tasks" ref={root}>
      <div className="task-box">
        {goalDraft === null ? (
          <>
            <form className="task-add tellnote" onSubmit={add}>
              <input
                value={title}
                placeholder={t('tasks.add')}
                aria-label={t('tasks.addOrSearch')}
                onChange={(e) => setTitle(e.target.value)}
              />
              <button type="submit" aria-label={t('tasks.addButton')} disabled={!title.trim()}>
                <svg viewBox="0 0 24 24" aria-hidden="true">
                  <path d="M5 12h14" />
                  <path d="M13 6l6 6-6 6" />
                </svg>
              </button>
            </form>
            <Overflow
              className="task-add-more"
              label={t('tasks.moreWays')}
              items={[
                { label: t('goals.new'), run: () => setGoalDraft({ title: '', due: '' }) },
                {
                  label: t('tasks.order'),
                  children: SORTS.map((o) => ({
                    label: t(o.label),
                    checked: sort === o.id,
                    run: () => {
                      setSort(o.id)
                      keepSort(o.id)
                    },
                  })),
                },
              ]}
            />
          </>
        ) : (
          <form className="task-add tellnote task-goal-add" onSubmit={addGoal}>
            <input
              value={goalDraft.title}
              placeholder={t('goals.placeholder')}
              aria-label={t('goals.goal')}
              autoFocus
              onChange={(e) => setGoalDraft({ ...goalDraft, title: e.target.value })}
            />
            <input
              type="date"
              value={goalDraft.due}
              aria-label={t('goals.due')}
              onChange={(e) => setGoalDraft({ ...goalDraft, due: e.target.value })}
            />
            <button type="submit" disabled={!goalDraft.title.trim()}>
              {t('common.save')}
            </button>
            <button type="button" onClick={() => setGoalDraft(null)}>
              {t('common.cancel')}
            </button>
          </form>
        )}
      </div>
      {categories.length > 0 && (
        <div className="seg task-cats" role="group" aria-label={t('tasks.category')}>
          <button type="button" aria-pressed={filter === null} onClick={() => setFilter(null)}>
            {t('memory.all')}
          </button>
          {categories.map((c) => (
            <button
              key={c}
              type="button"
              aria-pressed={filter === c}
              onClick={() => setFilter(filter === c ? null : c)}
            >
              {c}
            </button>
          ))}
        </div>
      )}
      {nothingFound && <p className="task-empty task-hint">{t('tasks.addHint', { title: title.trim() })}</p>}
      {goals.length > 0 && (
        <section className="task-group goals">
          <h3 className="task-group-head">{t('goals.head')}</h3>
          <div className="task-list">
            {goals.map((goal) => (
              <GoalRow
                key={goal.id}
                goal={goal}
                open={openGoals.has(goal.id)}
                onToggle={() => toggleGoal(goal.id)}
                onEdit={(patch) => editGoal(goal, patch)}
                onClose={(state) => closeGoal(goal, state)}
              >
                {nodes
                  .filter(
                    (n) =>
                      n.goal_id === goal.id &&
                      visible(n) &&
                      (isLive(n) || leaving.some((l) => l.id === n.id)),
                  )
                  .sort(bySchedule)
                  .map((n) =>
                    row(
                      { node: n, leaving: leaving.find((l) => l.id === n.id)?.state ?? null },
                      'soon',
                      'g',
                    ),
                  )}
              </GoalRow>
            ))}
          </div>
        </section>
      )}
      {soon.length > 0 && (
        <section className="task-group soon" data-drag-list>
          {head.length > 0 && (
            <div className="task-list">{head.map((p) => row(p, 'soon', '', canDrag))}</div>
          )}
          {(head.length > 0 || canDrag) && (
            <hr
              className={head.length > 0 ? 'order-edge' : 'order-edge bare'}
              aria-hidden="true"
            />
          )}
          {rest.length > 0 && list(rest, 'soon', canDrag)}
        </section>
      )}
      {later.length > 0 && (
        <section className="task-group later">
          <button
            className="task-done-fold"
            aria-expanded={laterOpen}
            onClick={() => setShowLater((v) => !v)}
          >
            {t('tasks.later')} <span className="task-count">{later.length}</span>
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M9 6l6 6-6 6" />
            </svg>
          </button>
          {laterOpen && list(later, 'later')}
        </section>
      )}
      {doneToday.length > 0 && (
        <section className="task-group done">
          <button
            className="task-done-fold"
            aria-expanded={showDone}
            onClick={() => setShowDone((v) => !v)}
          >
            {t('tasks.doneToday')} <span className="task-count">{doneToday.length}</span>
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M9 6l6 6-6 6" />
            </svg>
          </button>
          {showDone && (
            <div className="task-list">
              {doneToday.map((n) => (
                <Row
                  key={n.id}
                  node={n}
                  group="done"
                  actions={actions}
                  leaving={null}
                  onGone={gone}
                  stepsOpen={false}
                  categories={categories}
                  goals={goals}
                />
              ))}
            </div>
          )}
        </section>
      )}
    </div>
  )
}

const DAY_MS = 24 * 60 * 60 * 1000

// Days between two dates by the calendar, not by the hours between them.
const daysUntil = (due: Date, now: Date) =>
  Math.round(
    (new Date(due.getFullYear(), due.getMonth(), due.getDate()).getTime() -
      new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime()) /
      DAY_MS,
  )

function dueLabel(due: Date, now: Date): string {
  const days = daysUntil(due, now)
  if (days === 0) return t('due.today')
  if (days === 1) return t('due.tomorrow')
  return t('due.on', { day: days < 7 ? format.weekday(due) : format.day(due, due) })
}

// Urgency is the date's colour; an urgent task with no date keeps a dot of it.
function Due({ task }: { task: Task }) {
  const urgent = task.urgency === 'high' || task.pressing
  if (task.due_at === null)
    return urgent ? <span className="task-dot" role="img" aria-label={t('tasks.urgent')} /> : null
  const due = new Date(task.due_at)
  if (Number.isNaN(due.getTime())) return null
  const now = new Date()
  if (due.getTime() < now.getTime()) return <span className="meta warn">{t('due.overdue')}</span>
  return <span className={urgent ? 'meta sun' : 'meta'}>{dueLabel(due, now)}</span>
}

const URGENCY_RANK: Record<TaskUrgency, number> = { high: 0, normal: 2, low: 3 }

// 0 high, 1 pressing, 2 normal, 3 low: the order Later reads by default.
export function urgencyRank(task: Pick<Task, 'urgency' | 'pressing'>): number {
  if (task.urgency === 'high') return 0
  if (task.pressing) return 1
  return URGENCY_RANK[task.urgency]
}

// The word sits in sun-ink after the title; overdue keeps rose through `Due`.
export function Urgent({ task }: { task: Pick<Task, 'urgency' | 'pressing' | 'due_at'> }) {
  const overdue = task.due_at !== null && new Date(task.due_at).getTime() < Date.now()
  if (task.urgency !== 'high' && !(task.pressing && !overdue)) return null
  return <span className="meta sun">{t('tasks.urgent')}</span>
}

// A date input speaks in calendar days; a deadline is the end of one.
function endOfDay(date: string): string | null {
  const [y, m, d] = date.split('-').map(Number)
  if (!y || !m || !d) return null
  return new Date(y, m - 1, d, 23, 59).toISOString()
}

function dateValue(iso: string | null): string {
  if (iso === null) return ''
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

function goalDue(goal: Goal): string | null {
  if (goal.due_at === null) return null
  const due = new Date(goal.due_at)
  return Number.isNaN(due.getTime()) ? null : format.day(due, due)
}

const PROGRESS_STEP = 5

// Where the phone sets progress, having no bar to drag.
const PROGRESS_PICKS = [25, 50, 75]

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
  quiet,
}: {
  task: Task
  value: number
  onSet?: (progress: number) => void
  quiet?: boolean
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
    <span className="task-left">{t('tasks.minLeft', { n: task.remaining_min })}</span>
  )

  const cls = `task-prog${quiet ? ' quiet' : ''}`

  if (!onSet)
    return (
      <span className={cls}>
        <span
          className="task-bar-wrap"
          role="progressbar"
          aria-valuenow={value}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuetext={t('tasks.percentDone', { n: value })}
          aria-label={t('tasks.progressOf', { title: task.title })}
        >
          {bar}
        </span>
        {left}
      </span>
    )

  return (
    <span className={cls}>
      <span
        ref={track}
        className="task-bar-wrap"
        role="slider"
        tabIndex={0}
        data-dragging={dragging || undefined}
        aria-valuenow={value}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuetext={t('tasks.percentDone', { n: value })}
        aria-label={t('tasks.progressOf', { title: task.title })}
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

type GoalEdit = 'title' | 'due' | null

function GoalRow({
  goal,
  open,
  onToggle,
  onEdit,
  onClose,
  children,
}: {
  goal: Goal
  open: boolean
  onToggle: () => void
  onEdit: (patch: { title?: string; due_at?: string | null }) => void
  onClose: (state: 'done' | 'dropped') => void
  children: ReactNode
}) {
  const [editing, setEditing] = useState<GoalEdit>(null)
  const [draft, setDraft] = useState('')
  const pct = goal.tasks === 0 ? 0 : Math.round((goal.done_tasks / goal.tasks) * 100)

  const start = (what: Exclude<GoalEdit, null>) => {
    setDraft(what === 'title' ? goal.title : dateValue(goal.due_at))
    setEditing(what)
  }
  const keep = (e: FormEvent) => {
    e.preventDefault()
    if (editing === 'title') {
      const text = draft.trim()
      if (text !== '' && text !== goal.title) onEdit({ title: text })
    } else if (editing === 'due') {
      onEdit({ due_at: draft === '' ? null : endOfDay(draft) })
    }
    setEditing(null)
  }

  const items: OverflowItem[] = [
    { label: t('goals.rename'), run: () => start('title') },
    { label: t('goals.setDue'), run: () => start('due') },
    { label: t('goals.markDone'), run: () => onClose('done') },
    { label: t('menu.drop'), kind: 'danger', run: () => onClose('dropped') },
  ]

  return (
    <div className="task-item goal-item" data-row={`goal${goal.id}`}>
      <div className="task-row goal-row">
        <button
          className="goal-chev"
          aria-expanded={open}
          aria-label={t('goals.tasksOf', { title: goal.title })}
          onClick={onToggle}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M9 6l6 6-6 6" />
          </svg>
        </button>
        <div className="task-body">
          <span className="task-title">{goal.title}</span>
          {goalDue(goal) && <span className="meta">{goalDue(goal)}</span>}
        </div>
        <div className="task-foot">
          <span className="task-prog">
            <span
              className="task-bar-wrap"
              role="progressbar"
              aria-valuenow={pct}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-label={t('tasks.progressOf', { title: goal.title })}
            >
              <span className="task-bar">
                <span className="task-bar-fill" style={{ width: `${pct}%` }} />
              </span>
            </span>
          </span>
        </div>
        <span className="task-acts">
          <Overflow
            className="task-more"
            row=".goal-row"
            label={t('memory.moreFor', { name: goal.title })}
            items={items}
          />
        </span>
      </div>
      {editing !== null && (
        <form className="task-inline" onSubmit={keep}>
          {editing === 'title' ? (
            <input
              value={draft}
              aria-label={t('goals.renameFor', { title: goal.title })}
              autoFocus
              onChange={(e) => setDraft(e.target.value)}
            />
          ) : (
            <input
              type="date"
              value={draft}
              aria-label={t('goals.dueFor', { title: goal.title })}
              autoFocus
              onChange={(e) => setDraft(e.target.value)}
            />
          )}
          <button type="submit">{t('common.save')}</button>
          <button type="button" onClick={() => setEditing(null)}>
            {t('common.cancel')}
          </button>
        </form>
      )}
      {open && <div className="goal-tasks">{children}</div>}
    </div>
  )
}

// Finishing a row plays out in the stylesheet — the bar fills, the row greys,
// the bar fades — and the row folds away once that has been seen.
const FINISH_HOLD_S = 1.15
const REVIVE_MS = 420

function Row({
  node,
  group,
  scope = '',
  actions,
  leaving,
  onGone,
  stepsOpen,
  categories,
  goals,
  drag = false,
}: {
  node: TaskNode
  group: Group
  // Keeps the motion keys apart where the same task is drawn twice.
  scope?: string
  actions: RowActions
  leaving: TaskState | null
  onGone: (id: number) => void
  stepsOpen: boolean
  categories: string[]
  goals: Goal[]
  drag?: boolean
}) {
  const item = useRef<HTMLDivElement>(null)
  const [reviving, setReviving] = useState(false)
  const [naming, setNaming] = useState<string | null>(null)
  const sheet = useMenuSheet()
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

  const live = !done && leaving === null
  const loose = live && steps.length === 0

  const items: OverflowItem[] = []
  if (live) items.push({ label: t('block.start'), run: () => actions.startFocus(node) })
  items.push(
    node.is_now
      ? { label: t('tasks.toLater'), run: () => actions.moveToLater(node) }
      : { label: t('tasks.toNow'), run: () => actions.moveToNow(node) },
  )
  items.push({
    label: t('block.announce'),
    children: ANNOUNCE.map((choice) => ({
      label: t(choice.label),
      run: () => actions.announce(node, choice.id),
      checked: node.notify === choice.id,
    })),
  })
  if (live)
    items.push(
      {
        label: t('tasks.category'),
        children: [
          ...categories.map((c) => ({
            label: c,
            run: () => actions.setCategory(node, c),
            checked: node.category === c,
          })),
          { label: t('announce.none'), run: () => actions.setCategory(node, ''), checked: node.category === '' },
          { label: t('tasks.newCategory'), run: () => setNaming('') },
        ],
      },
      {
        label: t('goals.goal'),
        children: [
          ...goals.map((g) => ({
            label: g.title,
            run: () => actions.setGoal(node, g.id),
            checked: node.goal_id === g.id,
          })),
          { label: t('announce.none'), run: () => actions.setGoal(node, null), checked: node.goal_id === null },
        ],
      },
      {
        label: t('tasks.sort.urgency'),
        children: (['low', 'normal', 'high'] as const).map((u) => ({
          label: t(`urgency.${u}`),
          run: () => actions.setUrgency(node, u),
          checked: node.urgency === u,
        })),
      },
    )
  if (steps.length > 0) items.push({ label: t('tasks.mergeSteps'), run: () => actions.mergeSteps(node) })
  if (sheet && loose)
    items.push({
      label: t('tasks.progress'),
      children: [
        ...PROGRESS_PICKS.map((p) => ({
          label: format.number(p / 100, { style: 'percent' }),
          run: () => actions.setProgress(node, p),
        })),
        { label: t('menu.done'), run: () => actions.complete(node) },
      ],
    })
  items.push({ label: t('menu.drop'), kind: 'danger', run: () => actions.drop(node) })

  const progress = finished ? 100 : nodeProgress(node)
  const speaks = progress > 0

  return (
    <div
      className="task-item"
      ref={item}
      data-row={`${leaving ? 'x' : 't'}${scope}${node.id}`}
      data-leaving={leaving ?? undefined}
      data-finished={finished || undefined}
      data-drag={drag || undefined}
    >
      <div className="task-row">
        <Tick
          checked={finished}
          label={t(done ? 'tasks.markUndone' : 'tasks.tick', { text: node.title })}
          onClick={() => (done ? reopen() : actions.complete(node))}
        />
        <div className="task-body">
          <span className="task-title">{shownTitle(node)}</span>
          {steps.length > 0 && (
            <button
              className="task-fold"
              aria-expanded={stepsOpen}
              aria-label={parentSub(node)}
              onClick={() => actions.toggleSteps(node.id)}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M9 6l6 6-6 6" />
              </svg>
            </button>
          )}
        </div>
        <div className="task-foot">
          {!done && (
            <span className="task-meta">
              <Due task={node} />
            </span>
          )}
          {(speaks || !sheet) && (
            <Progress
              task={node}
              value={progress}
              quiet={!speaks}
              onSet={loose ? (p) => actions.setProgress(node, p) : undefined}
            />
          )}
        </div>
        <span className="task-acts">
          {!done && (
            <Overflow
              className="task-more"
              row=".task-row"
              label={t('memory.moreFor', { name: node.title })}
              items={items}
            />
          )}
          {node.is_now && !sheet && (
            <button
              className="task-start"
              data-tip={t('block.start')}
              aria-label={t('block.startFor', { name: focusTarget(node).title })}
              onClick={() => actions.startFocus(node)}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M9 7.5v9l7-4.5z" />
              </svg>
            </button>
          )}
        </span>
      </div>
      {naming !== null && (
        <form
          className="task-inline"
          onSubmit={(e) => {
            e.preventDefault()
            const text = naming.trim()
            if (text !== '') actions.setCategory(node, text)
            setNaming(null)
          }}
        >
          <input
            value={naming}
            placeholder={t('tasks.newCategoryPlaceholder')}
            aria-label={t('tasks.categoryFor', { title: node.title })}
            autoFocus
            onChange={(e) => setNaming(e.target.value)}
          />
          <button type="submit">{t('common.save')}</button>
          <button type="button" onClick={() => setNaming(null)}>
            {t('common.cancel')}
          </button>
        </form>
      )}
      {steps.length > 0 && stepsOpen && (
        <ul className="task-steps">
          {steps.map((c) => (
            <li key={c.id} className={`task-step${c.state === 'done' ? ' done' : ''}`}>
              <Tick
                checked={c.state === 'done'}
                label={t(c.state === 'done' ? 'tasks.markUndone' : 'tasks.tick', { text: c.title })}
                onClick={() =>
                  c.state === 'done' ? actions.reopenStep(c) : actions.complete(node, c)
                }
              />
              <span className="task-step-title">{c.title}</span>
              {c.duration_min !== null && (
                <span className="task-step-min meta">{round5(c.duration_min)} min</span>
              )}
              {(shown(c) > 0 || !sheet) && (
                <Progress
                  task={c}
                  value={shown(c)}
                  quiet={shown(c) === 0}
                  onSet={live && c.state !== 'done' ? (p) => actions.setProgress(c, p) : undefined}
                />
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
