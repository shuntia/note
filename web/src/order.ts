import { useEffect, type RefObject } from 'react'
import { flushSync } from 'react-dom'
import { reducedMotion } from './motion'
import { holds } from './tree'
import type { DayItem, TaskNode } from './types'

// The day's open task items in list order: the order Soon lifts and drags.
export const openTaskItems = (items: DayItem[]): number[] =>
  items.flatMap((i) =>
    i.kind === 'task' && i.task !== undefined && (i.state === 'open' || i.state === 'in_progress') ? [i.task] : [],
  )

// Soon's rows with today's order on top: each ordered item lifts the task that
// holds it, once, and the other rows keep their order beneath.
export function withOrder(
  soon: TaskNode[],
  live: TaskNode[],
  order: number[],
): { rows: TaskNode[]; ordered: number } {
  const top: TaskNode[] = []
  for (const id of order) {
    const found = live.find((n) => holds(n, id))
    if (found && !top.includes(found)) top.push(found)
  }
  return { rows: [...top, ...soon.filter((n) => !top.includes(n))], ordered: top.length }
}

const itemsOf = (node: TaskNode, order: number[]) => {
  const held = order.filter((id) => holds(node, id))
  return held.length > 0 ? held : [node.id]
}

// The order once the row at `from` is let go at `to` (an index among the other
// rows), above the order's edge when `into`.
export function dropped(
  order: number[],
  rows: TaskNode[],
  ordered: number,
  from: number,
  to: number,
  into: boolean,
): number[] {
  const kept = from < ordered ? ordered - 1 : ordered
  const seq = rows.slice()
  const [moved] = seq.splice(from, 1)
  seq.splice(into ? Math.min(to, kept) : Math.max(to, kept), 0, moved)
  return seq.slice(0, kept + (into ? 1 : 0)).flatMap((n) => itemsOf(n, order))
}

const HOLD_MS = 450
const SLOP_PX = 8
const SETTLE_MS = 180

// `list` is the `data-drag-list` value of the list the row was lifted in.
export type Drop = { from: number; to: number; into: boolean; list?: string }

// A row or the order's edge, where it was at the lift; `left` tells apart the columns of a list that flows into several.
export type Slot = { top: number; height: number; left?: number; edge?: boolean }

const columnOf = (s: Slot) => Math.round(s.left ?? 0)

// The column of `slots` nearest `left`, or every slot when `left` is not given.
function inColumn(slots: Slot[], left: number | undefined): number[] {
  const all = slots.map((_, i) => i)
  if (left === undefined || slots.length === 0) return all
  const near = slots.map(columnOf).reduce((a, b) => (Math.abs(b - left) < Math.abs(a - left) ? b : a))
  return all.filter((i) => columnOf(slots[i]) === near)
}

// Where the gap opens for the lifted row's middle at `y` (and left edge at `x` across columns): before slot `at`,
// which is `to` rows in, and above the edge when `into`.
export function gapAt(y: number, slots: Slot[], x?: number): { at: number; to: number; into: boolean } {
  const column = inColumn(slots, x)
  const found = column.find((i) => y < slots[i].top + slots[i].height / 2)
  const at = found ?? (column.length > 0 ? column[column.length - 1] + 1 : slots.length)
  const edge = slots.findIndex((b) => b.edge)
  const to = slots.slice(0, at).filter((b) => !b.edge).length
  return { at, to, into: edge !== -1 && at <= edge }
}

// The drop for a row lifted with its middle at `from.mid` and its left edge at `from.left`, let go `moved` away;
// null when let go where it was lifted.
export function landing(
  moved: { dx: number; dy: number },
  from: { left: number; mid: number },
  slots: Slot[],
): Omit<Drop, 'from'> | null {
  if (Math.hypot(moved.dx, moved.dy) <= SLOP_PX) return null
  const { to, into } = gapAt(from.mid + moved.dy, slots, from.left + moved.dx)
  return { to, into }
}

// The fewest-high split of `heights` into at most `count` columns read top to bottom: the index each column starts at.
function balanced(heights: number[], count: number): number[] {
  const fits = (h: number) => {
    const starts = [0]
    let used = 0
    heights.forEach((x, i) => {
      if (used > 0 && used + x > h) {
        starts.push(i)
        used = 0
      }
      used += x
    })
    return starts
  }
  const runs = new Set<number>()
  heights.forEach((_, i) => {
    let run = 0
    for (const x of heights.slice(i)) runs.add((run += x))
  })
  const tallest = [...runs].sort((a, b) => a - b).find((h) => fits(h).length <= count)
  return fits(tallest ?? Infinity)
}

// Where each of `seq` (a list's rows and edge, the lifted one at `from`) stands once the lifted one moves to `at`
// among the others. The rows on each side of the edge fill the list's columns top to bottom, and the edge runs
// across all of them between the two.
export function arranged(seq: Slot[], from: number, at: number): { left: number; top: number }[] {
  const order = seq.map((_, i) => i).filter((i) => i !== from)
  order.splice(at, 0, from)
  const lefts = [...new Set(seq.map(columnOf))].sort((a, b) => a - b)
  const bottom = (s: Slot) => s.top + s.height
  const edge = seq.findIndex((s) => s.edge)
  const gaps = seq.flatMap((s, i) =>
    i > 0 && !s.edge && !seq[i - 1].edge && columnOf(s) === columnOf(seq[i - 1]) ? [s.top - bottom(seq[i - 1])] : [],
  )
  const rowGap = gaps.length > 0 ? Math.max(0, Math.min(...gaps)) : 0
  const out = seq.map(() => ({ left: 0, top: 0 }))
  const flow = (rows: number[], top: number) => {
    const starts = balanced(rows.map((i) => seq[i].height + rowGap), lefts.length)
    let deepest = top
    starts.forEach((start, c) => {
      let y = top
      for (const i of rows.slice(start, starts[c + 1])) {
        out[i] = { left: lefts[c], top: y }
        y += seq[i].height + rowGap
      }
      deepest = Math.max(deepest, y - rowGap)
    })
    return deepest
  }
  const cut = edge === -1 ? order.length : order.indexOf(edge)
  const top = Math.min(...seq.map((s) => s.top))
  const head = order.slice(0, cut)
  const end = flow(head, top)
  if (edge === -1) return out
  const before = seq.filter((s, i) => !s.edge && i < edge)
  const after = seq.filter((s, i) => !s.edge && i > edge)
  const above = before.length > 0 ? seq[edge].top - Math.max(...before.map(bottom)) : null
  const below = after.length > 0 ? Math.min(...after.map((s) => s.top)) - bottom(seq[edge]) : null
  const edgeTop = head.length > 0 ? end + (above ?? below ?? 0) : top
  out[edge] = { left: lefts[0], top: edgeTop }
  flow(order.slice(cut + 1), edgeTop + seq[edge].height + (below ?? above ?? 0))
  return out
}

// Whether the drop changes the order, applying it when `commit`.
export type DropTo = (d: Drop, commit: boolean) => boolean

type Timer = ReturnType<typeof setTimeout>

type Press = { id: number; el: HTMLElement; list: HTMLElement; x: number; y: number }

type Lifted = {
  id: number
  el: HTMLElement
  list: HTMLElement
  rows: HTMLElement[]
  from: number
  x: number
  y: number
  // The list's rows and edge where they stood at the lift, the lifted row at `at`.
  seq: { el: HTMLElement; slot: Slot }[]
  at: number
  across: boolean
  key: string | undefined
}

// A long press lifts a `[data-drag]` row inside a `[data-drag-list]` of `doc`; the
// other rows of that same list, not of lists nested in it, and the list's
// `.order-edge` part to open a gap under the pointer, and the row settles into that
// gap when let go. A press inside a `.task-steps` list lifts only one of its own
// rows. Off while `drop` holds null. Returns the teardown.
export function liftDrag(doc: Document, drop: RefObject<DropTo | null>): () => void {
  let timer: Timer | undefined
  let settleTimer: Timer | undefined
  let press: Press | null = null
  let lifted: Lifted | null = null
  let settling: Lifted | null = null
  const own = (list: HTMLElement) => (el: HTMLElement) => el.closest('[data-drag-list]') === list
  const rowsOf = (list: HTMLElement) => [...list.querySelectorAll<HTMLElement>('[data-drag]')].filter(own(list))
  const box = (l: Lifted) => l.seq[l.at].slot
  const slotsOf = (l: Lifted) => l.seq.filter((_, i) => i !== l.at).map((p) => p.slot)
  const fromOf = (l: Lifted) => ({ left: box(l).left ?? 0, mid: box(l).top + box(l).height / 2 })
  const movedOf = (l: Lifted, e: PointerEvent) => ({ dx: l.across ? e.clientX - l.x : 0, dy: e.clientY - l.y })
  const place = (l: Lifted, i: number, to: { left: number; top: number }) => {
    const { el, slot } = l.seq[i]
    el.style.transform = `translate(${to.left - (slot.left ?? 0)}px, ${to.top - slot.top}px)`
  }
  const settleMs = () => (reducedMotion() ? 0 : SETTLE_MS)
  const unchanged = (l: Lifted) => {
    const now = rowsOf(l.list)
    return l.el.isConnected && now.length === l.rows.length && now.every((r, i) => r === l.rows[i])
  }

  // Moves the other rows aside for the gap under the pointer; returns where each would stand.
  const part = (l: Lifted, e: PointerEvent) => {
    const moved = movedOf(l, e)
    const from = fromOf(l)
    const { at } = gapAt(from.mid + moved.dy, slotsOf(l), from.left + moved.dx)
    const to = arranged(l.seq.map((p) => p.slot), l.at, at)
    to.forEach((spot, i) => i !== l.at && place(l, i, spot))
    return to
  }
  const clear = (l: Lifted) => {
    for (const p of l.seq) p.el.style.transform = ''
    delete l.el.dataset.lifted
    delete l.el.dataset.settling
    delete l.list.dataset.dragging
  }
  const finish = (l: Lifted, then: () => void) => {
    lifted = null
    settling = l
    l.el.dataset.settling = ''
    settleTimer = setTimeout(() => {
      settling = null
      flushSync(() => {
        then()
        clear(l)
      })
    }, settleMs())
  }
  const putBack = (l: Lifted) => {
    for (const p of l.seq) p.el.style.transform = ''
    finish(l, () => undefined)
  }
  const lift = () => {
    if (!press || lifted || !drop.current) return
    const { id, el, list, x, y } = press
    press = null
    const rows = rowsOf(list)
    const from = rows.indexOf(el)
    if (from === -1) return
    const seq = [...list.querySelectorAll<HTMLElement>('[data-drag], .order-edge')]
      .filter((other) => other.matches('.order-edge') || own(list)(other))
      .map((other) => {
        const r = other.getBoundingClientRect()
        return { el: other, slot: { top: r.top, height: r.height, left: r.left, edge: other.matches('.order-edge') } }
      })
    const across = new Set(seq.map((p) => Math.round(p.slot.left))).size > 1
    const at = seq.findIndex((p) => p.el === el)
    lifted = { id, el, list, rows, from, x, y, seq, at, across, key: list.dataset.dragList }
    el.dataset.lifted = ''
    list.dataset.dragging = ''
    el.style.transform = 'scale(1.02)'
    navigator.vibrate?.(10)
  }
  const down = (e: PointerEvent) => {
    clearTimeout(timer)
    if (press) {
      press = null
      return
    }
    if (e.button !== 0 || !drop.current || settling || lifted) return
    const target = e.target as HTMLElement
    if (target.closest('button, input, a, [role="slider"]')) return
    const el = target.closest<HTMLElement>('[data-drag]')
    const list = el?.closest<HTMLElement>('[data-drag-list]')
    if (!el || !list || target.closest('.task-steps') !== el.closest('.task-steps')) return
    press = { id: e.pointerId, el, list, x: e.clientX, y: e.clientY }
    timer = setTimeout(lift, HOLD_MS)
  }
  const move = (e: PointerEvent) => {
    const strayed = press && Math.hypot(e.clientX - press.x, e.clientY - press.y) > SLOP_PX
    if (press?.id === e.pointerId && strayed) {
      clearTimeout(timer)
      press = null
    }
    if (lifted?.id !== e.pointerId) return
    e.preventDefault()
    const { dx, dy } = movedOf(lifted, e)
    lifted.el.style.transform = `translate(${dx}px, ${dy}px) scale(1.02)`
    part(lifted, e)
  }
  const up = (e: PointerEvent) => {
    if (press?.id === e.pointerId) {
      clearTimeout(timer)
      press = null
    }
    const l = lifted
    if (l?.id !== e.pointerId) return
    const land = landing(movedOf(l, e), fromOf(l), slotsOf(l))
    const d = land && { from: l.from, ...land, ...(l.key !== undefined && { list: l.key }) }
    if (!d || !unchanged(l) || !drop.current?.(d, false)) return putBack(l)
    place(l, l.at, part(l, e)[l.at])
    finish(l, () => {
      if (unchanged(l)) drop.current?.(d, true)
    })
  }
  const cancel = (e: PointerEvent) => {
    if (press?.id === e.pointerId) {
      clearTimeout(timer)
      press = null
    }
    if (lifted?.id === e.pointerId) putBack(lifted)
  }
  const still = (e: TouchEvent) => {
    if (lifted) e.preventDefault()
  }
  // A touch long press raises its own contextmenu, which would open the row's menu.
  const menu = (e: Event) => {
    if (!press && !lifted) return
    e.preventDefault()
    e.stopPropagation()
    if (press) {
      clearTimeout(timer)
      lift()
    }
  }
  const on = <K extends keyof DocumentEventMap>(
    type: K,
    fn: (e: DocumentEventMap[K]) => void,
    opts?: AddEventListenerOptions,
  ) => {
    doc.addEventListener(type, fn, opts)
    return () => doc.removeEventListener(type, fn, opts)
  }
  const offs = [
    on('pointerdown', down),
    on('pointermove', move, { passive: false }),
    on('pointerup', up),
    on('pointercancel', cancel),
    on('touchmove', still, { passive: false }),
    on('contextmenu', menu, { capture: true }),
  ]
  return () => {
    clearTimeout(timer)
    clearTimeout(settleTimer)
    for (const l of [lifted, settling]) if (l) clear(l)
    for (const off of offs) off()
  }
}

export function useLiftDrag(drop: RefObject<DropTo | null>) {
  useEffect(() => liftDrag(document, drop), [drop])
}
