import {
  memo,
  useCallback,
  useDeferredValue,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type RefObject,
} from 'react'
import { api, ApiError } from '../api'
import { latest } from '../coalesce'
import { dropped, openTaskItems, useLiftDrag, withOrder, type DropTo } from '../order'
import type { ViewProps } from '../app'
import { collapse, flip, settle } from '../motion-gsap'
import { reducedMotion } from '../motion'
import { Overflow, useMenuSheet, type OverflowItem } from '../overflow'
import { useStableCallback, useStableMethods } from '../stable'
import { Tick } from '../tick'
import { urgencyRank } from '../urgent'
import { dueBody, dueDraft, dueParts, goalDated, localDay, type DueBody } from '../due'
import {
  findNode,
  finish,
  firstOpenLeaf,
  leaves,
  liveChildren,
  mapNode,
  mapSubtree,
  postOrder,
  progressOf,
  subtree,
  unpinAbove,
  weighted,
  withUrgency,
} from '../tree'
import '../styles/tasks.css'
import type { Adjusted, Goal, NewTask, Task, TaskNode, TaskNotify, TaskState, TaskUpdate, TaskUrgency } from '../types'
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

// A tree reads as urgent and as due as its most pressing live node.
const liveNodes = (n: TaskNode) => subtree(n).filter(isLive)

function soonestDue(n: TaskNode): string | null {
  let best: string | null = null
  for (const x of liveNodes(n)) if (earlier(x.effective_due_at, best) < 0) best = x.effective_due_at
  return best
}

const rankOf = (n: TaskNode) =>
  Math.min(...[n, ...liveNodes(n)].map((x) => urgencyRank({ urgency: x.effective_urgency, pressing: x.pressing })))

const bySchedule = (a: TaskNode, b: TaskNode) =>
  earlier(a.scheduled_at, b.scheduled_at) || earlier(soonestDue(a), soonestDue(b)) || newest(a, b)

const byUrgency = (a: TaskNode, b: TaskNode) =>
  rankOf(a) - rankOf(b) || earlier(soonestDue(a), soonestDue(b)) || newest(a, b)

const COMPARE: Record<SortKey, (a: TaskNode, b: TaskNode) => number> = {
  schedule: (a, b) => rankOf(a) - rankOf(b) || bySchedule(a, b),
  due: (a, b) => earlier(soonestDue(a), soonestDue(b)) || newest(a, b),
  urgency: byUrgency,
  newest,
  category: bySchedule,
}

// Every word has to land somewhere on the task or a title under it for it to stay in the list.
function matches(node: TaskNode, words: string[]): boolean {
  if (words.length === 0) return true
  const hay = [node.description, node.notes, node.category, node.goal_title ?? '', ...subtree(node).map((n) => n.title)]
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

// The prior states an undo puts back, in the order it sends them.
type Snapshot = { id: number; state: TaskState; progress: number }[]

const isLive = (t: Task) => t.state === 'open' || t.state === 'in_progress'

// The server only stores multiples of five; rounding here means no display path
// can ever show a duration the user could not have been offered.
const round5 = (min: number) => Math.max(5, Math.round(min / 5) * 5)

function parentSub(node: TaskNode): string {
  const all = leaves(node)
  return t('tasks.stepsDone', { done: all.filter((l) => l.state === 'done').length, count: all.length })
}

// What creating a node again takes: its own size when it is a leaf, its own due, and its tree.
function regrow(n: TaskNode): NewTask {
  const kids = liveChildren(n)
  return {
    title: n.title,
    ...(kids.length === 0 && n.duration_min !== null && { duration_min: n.duration_min }),
    ...(n.due_on !== null ? { due_on: n.due_on } : n.due_at !== null && { due_at: n.due_at }),
    ...(kids.length > 0 && { children: kids.map(regrow) }),
  }
}

const snapOf = (ns: TaskNode[]): Snapshot => ns.map((n) => ({ id: n.id, state: n.state, progress: n.progress }))

const nowCount = (nodes: TaskNode[], except?: TaskNode) =>
  nodes.filter((n) => n !== except).flatMap(liveNodes).filter((n) => n.is_now).length

// 'YYYY-MM-DD' for a whole day, else an instant.
function clampDay(to: unknown): string {
  if (typeof to !== 'string') return ''
  const day = /^(\d{4})-(\d{2})-(\d{2})$/.exec(to)
  return day ? format.day(new Date(+day[1], +day[2] - 1, +day[3])) : format.isoDay(to)
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
  const now = live.filter((n) => liveNodes(n).some((x) => x.is_now)).slice(0, NOW_CAP)
  const inNow = new Set(now.map((n) => n.id))
  const soon = (n: TaskNode) =>
    inNow.has(n.id) ||
    near(n.scheduled_at, today) ||
    liveNodes(n).some((x) => x.effective_urgency === 'high' || x.pressing || near(x.effective_due_at, today))
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

const NO_TASKS: TaskNode[] = []

const shownBy = (filter: string | null, words: string[]) => (n: TaskNode) =>
  (filter === null || n.category === filter) && matches(n, words)

// The goal's tasks that pass the filters, open and finished, each weighed by its minutes,
// and how many there were.
function goalProgress(
  goal: Goal,
  nodes: TaskNode[],
  filter: string | null,
  words: string[],
): { pct: number; parts: number } {
  const visible = shownBy(filter, words)
  const mine = nodes.filter((n) => n.goal_id === goal.id && n.parent_id === null && n.state !== 'dropped')
  const onScreen = new Set(mine.map((n) => n.id))
  const finished = goal.finished.filter(
    (f) =>
      !onScreen.has(f.id) &&
      (filter === null || f.category === filter) &&
      words.every((w) => `${f.title} ${f.category} ${goal.title}`.toLowerCase().includes(w)),
  )
  const parts = [
    ...mine.filter(visible).map((n) => ({ min: n.duration_min, pct: progressOf(n) })),
    ...finished.map((f) => ({ min: f.duration_min, pct: 100 })),
  ]
  return { pct: weighted(parts), parts: parts.length }
}

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
  return mapNode(nodes, t.id, (n) => ({ ...n, ...t, children: n.children }))
}

const stamped = (state: TaskState, progress?: number) => {
  const stamp = new Date().toISOString()
  return (t: TaskNode): TaskNode => ({
    ...t,
    state,
    updated_at: stamp,
    progress: state === 'done' ? 100 : (progress ?? t.progress),
  })
}

function withState(nodes: TaskNode[], id: number, state: TaskState, progress?: number): TaskNode[] {
  const put = stamped(state, progress)
  return mapNode(nodes, id, (n) => ({ ...put(n), children: n.children }))
}

// `demoted_from_now` rides on any write, not only one that set `is_now`.
function mergeUpdate(nodes: TaskNode[], u: TaskUpdate): TaskNode[] {
  const { parent, demoted_from_now, adjusted: _adjusted, ...task } = u
  let next = withTask(nodes, task)
  if (parent) next = withTask(next, parent)
  for (const id of demoted_from_now ?? []) next = mapNode(next, id, (n) => ({ ...n, is_now: false }))
  return next
}

// A finished or dropped task keeps its place in the list until it has folded away.
type Leaving = { id: number; group: Exclude<Group, 'done'>; index: number; state: TaskState; node: TaskNode }

type Placed = { node: TaskNode; leaving: TaskState | null }

// A leaving row is drawn from the node it left as, since a reload no longer lists a dropped one.
function placed(list: TaskNode[], nodes: TaskNode[], leaving: Leaving[], group: Group): Placed[] {
  const out: Placed[] = list.map((node) => ({ node, leaving: null }))
  for (const l of leaving) {
    if (l.group !== group) continue
    const node = nodes.find((n) => n.id === l.id) ?? l.node
    out.splice(Math.min(l.index, out.length), 0, { node, leaving: l.state })
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
    const base = root.current?.getBoundingClientRect()
    root.current?.querySelectorAll<HTMLElement>('[data-row]').forEach((el) => {
      const key = el.dataset.row as string
      const r = el.getBoundingClientRect()
      at.set(key, new DOMRect(r.left - (base?.left ?? 0), r.top - (base?.top ?? 0), r.width, r.height))
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
  setDue: (node: TaskNode, due: DueBody) => void
  setGoal: (node: TaskNode, goal_id: number | null) => void
  setProgress: (task: Task, progress: number) => void
  complete: (node: TaskNode) => void
  reopen: (node: TaskNode) => void
  reorder: (node: TaskNode, position: number) => void
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
  const [filter, setFilter] = useState<string | null>(null)
  const [sort, setSort] = useState<SortKey>(storedSort)
  const [goalDraft, setGoalDraft] = useState<{ title: string; due: string; time: string } | null>(null)
  const [runOrder, setRunOrder] = useState<number[]>([])
  const dropRef = useRef<DropTo | null>(null)
  useLiftDrag(dropRef)
  const seeded = useRef(false)
  const unfinished = useRef(new Map<number, number>())
  const root = useRef<HTMLDivElement>(null)
  const { mark } = useRowMotion(root, leaving.length > 0)
  // The list follows the box a beat behind, so typing never waits on the rows.
  const query = useDeferredValue(title)
  const shown = useMemo(
    () => (nodes ? shownGroups(nodes, filter, query, sort, runOrder) : null),
    [nodes, filter, query, sort, runOrder],
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
      .day(localDay(new Date()))
      .then((d) => setRunOrder(openTaskItems(d.items)))
      .catch(() => setRunOrder([]))
    newest(api.tasks())
      .then((ts) => {
        if (!ts) return
        setNodes(ts)
        setFailed(false)
        if (seeded.current) return
        seeded.current = true
        setOpenSteps(
          new Set(ts.filter((t) => t.children.length > 0 && liveNodes(t).some((x) => x.is_now)).map((t) => t.id)),
        )
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
        due_at?: string | null
        due_on?: string | null
        position?: number
      },
    ) => {
      try {
        const updated = await api.patchTask(id, body)
        setNodes((ns) => (ns ? mergeUpdate(ns, updated) : ns))
        const clamp = updated.adjusted?.find((a: Adjusted) => a.field === 'due')
        if (clamp)
          notify(t('tasks.dueClamped', { date: clampDay(clamp.to), title: clamp.by_title ?? updated.title }))
        load()
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
        ns ? snap.reduce((acc, s) => unpinAbove(withState(acc, s.id, s.state, s.progress), s.id), ns) : ns,
      )
      for (const s of snap) await patch(s.id, { state: s.state, progress: s.progress })
    },
    [mark, patch],
  )

  const markLeaving = (node: TaskNode, state: TaskState) => {
    if (!shown) return
    const soon = shown.soon.findIndex((n) => n.id === node.id)
    const index = soon === -1 ? shown.later.findIndex((n) => n.id === node.id) : soon
    if (index === -1) return
    setLeaving((ls) => [...ls, { id: node.id, group: soon === -1 ? 'later' : 'soon', index, state, node }])
  }
  const gone = useCallback(
    (id: number) => {
      mark()
      setLeaving((ls) => ls.filter((l) => l.id !== id))
    },
    [mark],
  )

  // Finishing a node takes its unfinished descendants with it and may finish its
  // ancestors, so undo has to put the whole cascade back.
  const complete = (node: TaskNode) => {
    if (!nodes) return
    const { roots, changed } = finish(nodes, node.id, stamped('done'))
    const snap = snapOf(changed)
    for (const s of snap) unfinished.current.set(s.id, s.progress)
    const root = roots.find((r) => r.id === node.root_id || r.id === node.id)
    if (root?.state === 'done') markLeaving(root, 'done')
    setNodes(unpinAbove(roots, node.id))
    void patch(node.id, { state: 'done' })
    notify(t('tasks.doneToast', { title: node.title }), {
      label: t('toast.undo'),
      run: () => void restore(snap),
      windowMs: UNDO_MS,
    })
  }

  // Reopening a finished node brings its descendants back too, so no count can
  // claim work is done under a node that is open again. Each goes back to the
  // progress it had before it was finished here, when that is known.
  const reopened = (t: Task) => ({
    id: t.id,
    state: 'open' as TaskState,
    progress: unfinished.current.get(t.id) ?? t.progress,
  })
  const reopen = (node: TaskNode) =>
    void restore(postOrder(node).filter((n) => n.state !== 'dropped').map(reopened))

  // Dropping a node drops everything under it, finished parts included, and a
  // dropped node never comes back on its own, so undo names each of them, the
  // node first so its descendants come back under a live parent.
  const drop = (node: TaskNode) => {
    const snap = snapOf(subtree(node).filter((n) => n.state !== 'dropped'))
    if (node.parent_id === null) markLeaving(node, 'dropped')
    const put = stamped('dropped')
    setNodes((ns) => (ns ? mapNode(ns, node.id, (n) => mapSubtree(n, put)) : ns))
    void patch(node.id, { state: 'dropped' })
    notify(t('tasks.droppedToast', { title: node.title }), {
      label: t('toast.undo'),
      run: () => void restore(snap),
      windowMs: UNDO_MS,
    })
  }

  const reorder = (node: TaskNode, position: number) => {
    if (node.parent_id === null) return
    mark()
    setNodes((ns) =>
      ns
        ? mapNode(ns, node.parent_id as number, (p) => {
            const kids = liveChildren(p).filter((c) => c.id !== node.id)
            kids.splice(position, 0, node)
            return { ...p, children: kids.map((c, i) => ({ ...c, position: i })) }
          })
        : ns,
    )
    void patch(node.id, { position })
  }

  const setProgress = (task: Task, progress: number) => {
    setNodes((ns) => (ns ? unpinAbove(withTask(ns, { ...task, progress, progress_set: true }), task.id) : ns))
    void patch(task.id, { progress })
  }

  const announce = (node: TaskNode, notify: TaskNotify) => {
    setNodes((ns) => (ns ? mapNode(ns, node.id, (n) => ({ ...n, notify })) : ns))
    void patch(node.id, { notify })
  }

  // A category rides on the root and is read back by everything under it.
  const setCategory = (node: TaskNode, category: string) => {
    setNodes((ns) => (ns ? mapNode(ns, node.id, (n) => mapSubtree(n, (c) => ({ ...c, category }))) : ns))
    if (filter !== null && filter !== category) setFilter(null)
    void patch(node.id, { category })
  }

  const setDue = (node: TaskNode, due: DueBody) => void patch(node.id, due)

  const setUrgency = (node: TaskNode, urgency: TaskUrgency) => {
    setNodes((ns) => (ns ? mapNode(ns, node.id, (n) => withUrgency(n, urgency)) : ns))
    void patch(node.id, { urgency })
  }

  const setGoal = (node: TaskNode, goal_id: number | null) => {
    const goal_title = goals.find((g) => g.id === goal_id)?.title ?? null
    setNodes((ns) =>
      ns ? mapNode(ns, node.id, (n) => mapSubtree(n, (c) => ({ ...c, goal_id, goal_title }))) : ns,
    )
    void patch(node.id, { goal_id })
  }

  // A task carries the fold its group asks for: open in Now, closed in Later.
  const setNow = (node: TaskNode, is_now: boolean) => {
    setNodes((ns) => (ns ? mapNode(ns, node.id, (n) => ({ ...n, is_now })) : ns))
    setOpenSteps((open) => {
      const next = new Set(open)
      if (is_now) next.add(node.id)
      else next.delete(node.id)
      return next
    })
    void patch(node.id, { is_now })
  }

  const editGoal = (goal: Goal, patch: { title?: string } | DueBody) => {
    api
      .patchGoal(goal.id, patch)
      .then((g) => setGoals((gs) => gs.map((x) => (x.id === g.id ? g : x))))
      .catch(() => {
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
    const due = dueBody(draft.due, draft.time)
    if (due === null) return
    setGoalDraft(null)
    api
      .addGoal({ title: text, ...due })
      .then((g) => setGoals((gs) => [...gs, g]))
      .catch(() => notify(t('goals.addFailed')))
  }

  const toggleSteps = (id: number) =>
    setOpenSteps((open) => {
      const next = new Set(open)
      if (!next.delete(id)) next.add(id)
      return next
    })

  // The only reversal a flatten has is creating the tree under it again, so the
  // tree travels into the undo closure rather than being read back off a stale row.
  const mergeSteps = (node: TaskNode) => {
    const regrown = liveChildren(node).map(regrow)
    setNodes((ns) => (ns ? mapNode(ns, node.id, (n) => ({ ...n, children: [] })) : ns))
    api
      .flattenTask(node.id)
      .then(load)
      .catch(() => {
        notify(t('tasks.updateFailed'))
        load()
      })
    notify(t('tasks.mergedToast', { title: node.title }), {
      label: t('toast.undo'),
      run: () =>
        void (async () => {
          for (const { title, ...c } of regrown) await api.addTask(title, { parent_id: node.id, ...c })
        })()
          .catch(() => notify(t('tasks.splitFailed')))
          .finally(load),
      windowMs: UNDO_MS,
    })
  }

  // A root holding a Now node starts on that node's first open leaf.
  const startFocus = (node: TaskNode) => {
    const from = subtree(node).find((n) => n !== node && n.is_now && isLive(n)) ?? node
    const leaf = firstOpenLeaf(from) ?? from
    openNow({
      title: node.title,
      task_id: node.id,
      notes: leaf.notes,
      ...(leaf.id !== node.id && { leaf_id: leaf.id }),
      ...(leaf.duration_min !== null && { planned_min: round5(leaf.duration_min) }),
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
    const temp = -Date.now()
    const optimistic: TaskNode = {
      id: temp,
      title: text,
      description: '',
      state: 'open',
      source: 'manual',
      notes: '',
      duration_min: null,
      duration_source: 'none',
      parent_id: null,
      root_id: temp,
      depth: 0,
      position: 0,
      is_now,
      updated_at: new Date().toISOString(),
      due_at: null,
      due_on: null,
      effective_due_at: null,
      effective_due_on: null,
      due_from: null,
      external_id: null,
      url: '',
      notify: 'notify',
      category,
      urgency: 'normal',
      effective_urgency: 'normal',
      pressing: false,
      goal_id: null,
      goal_title: null,
      scheduled_at: null,
      progress: 0,
      progress_set: false,
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

  const actions = useStableMethods<RowActions>({
    setCategory,
    setUrgency,
    setDue,
    setGoal,
    setProgress,
    complete,
    reopen,
    reorder,
    moveToNow: (node) =>
      nodes && nowCount(nodes, node) >= NOW_CAP ? notify(t('tasks.nowFull')) : setNow(node, true),
    moveToLater: (node) => setNow(node, false),
    drop,
    mergeSteps,
    startFocus,
    announce,
    toggleSteps,
  })
  const onCompleteInGoal = useStableCallback((node: TaskNode) => actions.complete(node))
  const onEditGoal = useStableCallback(editGoal)
  const onCloseGoal = useStableCallback(closeGoal)
  const categories = useMemo(() => (nodes ? categoriesOf(nodes) : []), [nodes])

  // Each goal's open tasks that pass the filters, kept the same array while nothing in it changed.
  const goalTasks = useMemo(() => {
    const out = new Map<number, TaskNode[]>()
    if (!nodes) return out
    const visible = shownBy(filter, searchWords(query))
    for (const goal of goals)
      out.set(goal.id, nodes.filter((n) => n.goal_id === goal.id && isLive(n) && visible(n)).sort(bySchedule))
    return out
  }, [nodes, goals, filter, query])
  const goalPcts = useMemo(() => {
    const words = searchWords(query)
    return new Map(goals.map((g) => [g.id, nodes ? goalProgress(g, nodes, filter, words) : { pct: 0, parts: 0 }]))
  }, [nodes, goals, filter, query])

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
  const search = query.trim().toLowerCase()

  const soon = placed(shown?.soon ?? [], nodes, leaving, 'soon')
  const later = placed(shown?.later ?? [], nodes, leaving, 'later')
  // A task settles into Done today only once it has folded away: until then it is
  // still in the list it is leaving, and a row is never in two lists at once.
  const doneToday = g.doneToday.filter((n) => !leaving.some((l) => l.id === n.id))
  const nothingFound = search !== '' && soon.length === 0 && later.length === 0
  const laterOpen = showLater || search !== ''
  // Under a filter, a goal with nothing that passes it steps aside.
  const shownGoals =
    filter === null && search === '' ? goals : goals.filter((g) => (goalPcts.get(g.id)?.parts ?? 0) > 0)
  const orderedIds = new Set((shown?.soon ?? []).slice(0, shown?.ordered ?? 0).map((n) => n.id))
  const edgeAt = soon.findIndex((p) => !orderedIds.has(p.node.id))
  const head = edgeAt === -1 ? soon : soon.slice(0, edgeAt)
  const rest = edgeAt === -1 ? [] : soon.slice(edgeAt)
  const canDrag = filter === null && search === '' && leaving.length === 0
  const dragRows = [
    ...head,
    ...(sort === 'category' ? byCategory(rest, categories).flatMap(([, rows]) => rows) : rest),
  ].map((p) => p.node)
  const soonDrop: DropTo | null = canDrag
    ? ({ from, to, into }, commit) => {
        const next = dropped(runOrder, dragRows, head.length, from, to, into)
        const same = next.length === runOrder.length && next.every((id, i) => id === runOrder[i])
        if (same) return false
        if (!commit) return true
        mark()
        setRunOrder(next)
        api
          .setDayOrder(localDay(new Date()), next)
          .then((o) => setRunOrder(o.task_ids))
          .catch(() => {
            notify(t('tasks.updateFailed'))
            load()
          })
        return true
      }
    : null
  dropRef.current = (d, commit) => {
    if (d.list === 'soon') return soonDrop ? soonDrop(d, commit) : false
    const parent = d.list === undefined ? null : findNode(nodes, Number(d.list))
    const moved = parent ? liveChildren(parent)[d.from] : undefined
    if (!moved || d.to === d.from) return false
    if (commit) actions.reorder(moved, d.to)
    return true
  }

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
                { label: t('goals.new'), run: () => setGoalDraft({ title: '', due: '', time: '' }) },
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
            {goalDraft.due !== '' && (
              <input
                type="time"
                value={goalDraft.time}
                aria-label={t('due.time')}
                onChange={(e) => setGoalDraft({ ...goalDraft, time: e.target.value })}
              />
            )}
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
      {nothingFound && <p className="task-empty task-hint">{t('tasks.addHint', { title: query.trim() })}</p>}
      {shownGoals.length > 0 && (
        <section className="task-group goals" aria-label={t('goals.head')}>
          <div className="goal-strip">
            {shownGoals.map((goal) => (
              <GoalCard
                key={goal.id}
                goal={goal}
                pct={goalPcts.get(goal.id)?.pct ?? 0}
                tasks={goalTasks.get(goal.id) ?? NO_TASKS}
                onComplete={onCompleteInGoal}
                onEdit={onEditGoal}
                onClose={onCloseGoal}
              />
            ))}
          </div>
        </section>
      )}
      {soon.length > 0 && (
        <section className="task-group soon" data-drag-list="soon">
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

function dueLabel(day: Date, at: Date | null, now: Date): string {
  const days = daysUntil(day, now)
  const time = at === null ? null : format.clock(at)
  if (days === 0) return time === null ? t('due.today') : t('due.todayAt', { time })
  if (days === 1) return time === null ? t('due.tomorrow') : t('due.tomorrowAt', { time })
  const name = days < 7 ? format.weekday(day) : format.day(day, day)
  return time === null ? t('due.on', { day: name }) : t('due.onAt', { day: name, time })
}

// Urgency is the date's colour; an urgent task with no date keeps a dot of it.
// `own` shows only a due the node carries itself, and no dot.
function Due({ task, own = false }: { task: Task; own?: boolean }) {
  const urgent = task.effective_urgency === 'high' || task.pressing
  const dated = own ? task : { due_at: task.effective_due_at, due_on: task.effective_due_on }
  const due = dueParts(dated)
  if (due === null || dated.due_at === null)
    return urgent && !own ? <span className="task-dot" role="img" aria-label={t('tasks.urgent')} /> : null
  const now = new Date()
  if (new Date(dated.due_at).getTime() < now.getTime())
    return <span className="meta warn">{t('due.overdue')}</span>
  return <span className={urgent ? 'meta sun' : 'meta'}>{dueLabel(due.day, due.at, now)}</span>
}

function goalDue(goal: Goal): string | null {
  const due = dueParts(goalDated(goal))
  if (due === null) return null
  const day = format.day(due.day, due.day)
  return due.at === null ? day : `${day} ${format.clock(due.at)}`
}

// A date and an optional time; an empty time is the whole day.
function DueFields({
  date,
  time,
  label,
  onDate,
  onTime,
}: {
  date: string
  time: string
  label: string
  onDate: (v: string) => void
  onTime: (v: string) => void
}) {
  return (
    <>
      <input type="date" value={date} aria-label={label} autoFocus onChange={(e) => onDate(e.target.value)} />
      {date !== '' && (
        <input type="time" value={time} aria-label={t('due.time')} onChange={(e) => onTime(e.target.value)} />
      )}
    </>
  )
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

const GoalCard = memo(function GoalCard({
  goal,
  pct,
  tasks,
  onComplete,
  onEdit,
  onClose,
}: {
  goal: Goal
  pct: number
  tasks: TaskNode[]
  onComplete: (node: TaskNode) => void
  onEdit: (goal: Goal, patch: { title?: string } | DueBody) => void
  onClose: (goal: Goal, state: 'done' | 'dropped') => void
}) {
  const [editing, setEditing] = useState<GoalEdit>(null)
  const [draft, setDraft] = useState('')
  const [time, setTime] = useState('')

  const start = (what: Exclude<GoalEdit, null>) => {
    const due = dueDraft(goalDated(goal))
    setDraft(what === 'title' ? goal.title : due.date)
    setTime(due.time)
    setEditing(what)
  }
  const keep = (e: FormEvent) => {
    e.preventDefault()
    if (editing === 'title') {
      const text = draft.trim()
      if (text !== '' && text !== goal.title) onEdit(goal, { title: text })
    } else if (editing === 'due') {
      const due = dueBody(draft, time)
      if (due !== null) onEdit(goal, due)
    }
    setEditing(null)
  }

  const items: OverflowItem[] = [
    { label: t('goals.rename'), run: () => start('title') },
    { label: t('goals.setDue'), run: () => start('due') },
    { label: t('goals.markDone'), run: () => onClose(goal, 'done') },
    { label: t('menu.drop'), kind: 'danger', run: () => onClose(goal, 'dropped') },
  ]

  return (
    <div className="goal-card" data-row={`goal${goal.id}`}>
      <div className="goal-card-head">
        <span className="goal-card-title">{goal.title}</span>
        <Overflow
          className="task-more"
          row=".goal-card"
          label={t('memory.moreFor', { name: goal.title })}
          items={items}
        />
      </div>
      {goalDue(goal) && <span className="meta">{goalDue(goal)}</span>}
      <span
        className="task-bar"
        role="progressbar"
        aria-valuenow={pct}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-label={t('tasks.progressOf', { title: goal.title })}
      >
        <span className="task-bar-fill" style={{ width: `${pct}%` }} />
      </span>
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
            <DueFields
              date={draft}
              time={time}
              label={t('goals.dueFor', { title: goal.title })}
              onDate={setDraft}
              onTime={setTime}
            />
          )}
          <button type="submit">{t('common.save')}</button>
          <button type="button" onClick={() => setEditing(null)}>
            {t('common.cancel')}
          </button>
        </form>
      )}
      {tasks.length > 0 && (
        <ul className="goal-card-tasks" aria-label={t('goals.tasksOf', { title: goal.title })}>
          {tasks.map((n) => (
            <li key={n.id}>
              <Tick checked={false} label={t('tasks.tick', { text: n.title })} onClick={() => onComplete(n)} />
              <span className="goal-card-task">{shownTitle(n)}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
})

// Finishing a row plays out in the stylesheet — the bar fills, the row greys,
// the bar fades — and the row folds away once that has been seen.
const FINISH_HOLD_S = 1.15
const REVIVE_MS = 420

const Row = memo(function Row({
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
  const [dating, setDating] = useState<{ date: string; time: string } | null>(null)
  const sheet = useMenuSheet()
  const done = group === 'done'
  const finished = (done && !reviving) || leaving === 'done'
  const steps = done ? [] : liveChildren(node)

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
      { label: t('goals.setDue'), run: () => setDating(dueDraft(node)) },
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
  if (sheet && live)
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

  const progress = finished ? 100 : progressOf(node)
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
              onSet={live ? (p) => actions.setProgress(node, p) : undefined}
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
          {liveNodes(node).some((x) => x.is_now) && !sheet && (
            <button
              className="task-start"
              data-tip={t('block.start')}
              aria-label={t('block.startFor', { name: (firstOpenLeaf(node) ?? node).title })}
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
      {dating !== null && (
        <form
          className="task-inline"
          onSubmit={(e) => {
            e.preventDefault()
            const due = dueBody(dating.date, dating.time)
            if (due !== null) actions.setDue(node, due)
            setDating(null)
          }}
        >
          <DueFields
            date={dating.date}
            time={dating.time}
            label={t('goals.dueFor', { title: node.title })}
            onDate={(date) => setDating({ ...dating, date })}
            onTime={(time) => setDating({ ...dating, time })}
          />
          <button type="submit">{t('common.save')}</button>
          <button type="button" onClick={() => setDating(null)}>
            {t('common.cancel')}
          </button>
        </form>
      )}
      {steps.length > 0 && stepsOpen && <Steps parent={node} live={live} sheet={sheet} actions={actions} />}
    </div>
  )
})

// The rows under a parent, at any depth; a live row drags among its siblings.
function Steps({
  parent,
  live,
  sheet,
  actions,
}: {
  parent: TaskNode
  live: boolean
  sheet: boolean
  actions: RowActions
}) {
  return (
    <ul className="task-steps" data-drag-list={String(parent.id)}>
      {liveChildren(parent).map((c) => (
        <Step key={c.id} node={c} live={live} sheet={sheet} actions={actions} />
      ))}
    </ul>
  )
}

const Step = memo(function Step({
  node,
  live,
  sheet,
  actions,
}: {
  node: TaskNode
  live: boolean
  sheet: boolean
  actions: RowActions
}) {
  const [open, setOpen] = useState(false)
  const kids = liveChildren(node)
  const finished = node.state === 'done'
  const value = progressOf(node)
  return (
    <li className={`task-step${finished ? ' done' : ''}`} data-drag={live || undefined}>
      <Tick
        checked={finished}
        label={t(finished ? 'tasks.markUndone' : 'tasks.tick', { text: node.title })}
        onClick={() => (finished ? actions.reopen(node) : actions.complete(node))}
      />
      <span className="task-step-head">
        <span className="task-step-title">{node.title}</span>
        {kids.length > 0 && (
          <button
            className="task-fold"
            aria-expanded={open}
            aria-label={parentSub(node)}
            onClick={() => setOpen((o) => !o)}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M9 6l6 6-6 6" />
            </svg>
          </button>
        )}
      </span>
      <span className="task-step-min">
        {!finished && <Due task={node} own />}
        {node.duration_min !== null && <span className="meta">{round5(node.duration_min)} min</span>}
      </span>
      {(value > 0 || !sheet) && (
        <Progress
          task={node}
          value={value}
          quiet={value === 0}
          onSet={live && !finished ? (p) => actions.setProgress(node, p) : undefined}
        />
      )}
      {kids.length > 0 && open && <Steps parent={node} live={live} sheet={sheet} actions={actions} />}
    </li>
  )
})
