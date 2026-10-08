import { useEffect, type RefObject } from 'react'
import { flushSync } from 'react-dom'
import { reducedMotion } from './motion'
import type { TaskNode } from './types'

const holds = (node: TaskNode, id: number) => node.id === id || node.children.some((c) => c.id === id)

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

export type Drop = { from: number; to: number; into: boolean }

// A row or the order's edge other than the lifted row, where it was at the lift.
export type Slot = { top: number; height: number; edge?: boolean }

// Where the gap opens for the lifted row's middle at `y`: before slot `at`, which
// is `to` rows in, and above the edge when `into`.
export function gapAt(y: number, slots: Slot[]): { at: number; to: number; into: boolean } {
  const found = slots.findIndex((b) => y < b.top + b.height / 2)
  const at = found === -1 ? slots.length : found
  const edge = slots.findIndex((b) => b.edge)
  const to = slots.slice(0, at).filter((b) => !b.edge).length
  return { at, to, into: edge !== -1 && at <= edge }
}

// The drop for a row whose middle was at `mid` when lifted and let go `dy` away;
// null when let go where it was lifted.
export function landing(dy: number, mid: number, slots: Slot[]): Omit<Drop, 'from'> | null {
  if (Math.abs(dy) <= SLOP_PX) return null
  const { to, into } = gapAt(mid + dy, slots)
  return { to, into }
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
  y: number
  height: number
  top: number
  parts: { el: HTMLElement; slot: Slot; shift: number }[]
}

// A long press lifts a `[data-drag]` row inside a `[data-drag-list]` of `doc`; the
// other rows and the list's `.order-edge` part to open a gap under the pointer, and
// the row settles into that gap when let go. Off while `drop` holds null. Returns
// the teardown.
export function liftDrag(doc: Document, drop: RefObject<DropTo | null>): () => void {
  let timer: Timer | undefined
  let settleTimer: Timer | undefined
  let press: Press | null = null
  let lifted: Lifted | null = null
  let settling: Lifted | null = null
  const rowsOf = (list: HTMLElement) => [...list.querySelectorAll<HTMLElement>('[data-drag]')]
  const slotsOf = (l: Lifted) => l.parts.map((p) => p.slot)
  const midOf = (l: Lifted) => l.top + l.height / 2
  const settleMs = () => (reducedMotion() ? 0 : SETTLE_MS)
  const unchanged = (l: Lifted) => {
    const now = rowsOf(l.list)
    return l.el.isConnected && now.length === l.rows.length && now.every((r, i) => r === l.rows[i])
  }

  const part = (l: Lifted, y: number) => {
    const { at } = gapAt(midOf(l) + y - l.y, slotsOf(l))
    l.parts.forEach((p, i) => {
      p.el.style.transform = `translateY(${p.shift + (i >= at ? l.height : 0)}px)`
    })
    return at
  }
  const clear = (l: Lifted) => {
    for (const el of [l.el, ...l.parts.map((p) => p.el)]) el.style.transform = ''
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
    for (const el of [l.el, ...l.parts.map((p) => p.el)]) el.style.transform = ''
    finish(l, () => undefined)
  }
  const lift = () => {
    if (!press || lifted || !drop.current) return
    const { id, el, list, y } = press
    press = null
    const rows = rowsOf(list)
    const from = rows.indexOf(el)
    if (from === -1) return
    const box = el.getBoundingClientRect()
    let after = false
    const parts: Lifted['parts'] = []
    for (const other of list.querySelectorAll<HTMLElement>('[data-drag], .order-edge')) {
      if (other === el) {
        after = true
        continue
      }
      const r = other.getBoundingClientRect()
      const slot = { top: r.top, height: r.height, edge: other.matches('.order-edge') }
      parts.push({ el: other, slot, shift: after ? -box.height : 0 })
    }
    lifted = { id, el, list, rows, from, y, height: box.height, top: box.top, parts }
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
    if (target.closest('button, input, a, [role="slider"], .task-steps')) return
    const el = target.closest<HTMLElement>('[data-drag]')
    const list = el?.closest<HTMLElement>('[data-drag-list]')
    if (!el || !list) return
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
    lifted.el.style.transform = `translateY(${e.clientY - lifted.y}px) scale(1.02)`
    part(lifted, e.clientY)
  }
  const up = (e: PointerEvent) => {
    if (press?.id === e.pointerId) {
      clearTimeout(timer)
      press = null
    }
    const l = lifted
    if (l?.id !== e.pointerId) return
    const land = landing(e.clientY - l.y, midOf(l), slotsOf(l))
    const d = land && { from: l.from, ...land }
    if (!d || !unchanged(l) || !drop.current?.(d, false)) return putBack(l)
    const at = part(l, e.clientY)
    const closedTop = (p: Lifted['parts'][number]) => p.slot.top + p.shift
    const last = l.parts[l.parts.length - 1]
    const gapTop =
      at < l.parts.length
        ? closedTop(l.parts[at])
        : last
          ? closedTop(last) + last.slot.height
          : l.top
    l.el.style.transform = `translateY(${gapTop - l.top}px)`
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
