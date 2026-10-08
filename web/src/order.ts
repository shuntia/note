import { useEffect, type RefObject } from 'react'
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

export type Drop = { from: number; to: number; into: boolean }

// Where a row lifted at `liftY` lands when let go at `y`: its index among the
// other rows' boxes, and whether it is above the order's edge. Null when let go
// where it was lifted.
export function landing(
  liftY: number,
  y: number,
  others: { top: number; height: number }[],
  edgeTop: number | null,
): { to: number; into: boolean } | null {
  if (Math.abs(y - liftY) <= SLOP_PX) return null
  const at = others.findIndex((box) => y < box.top + box.height / 2)
  return { to: at === -1 ? others.length : at, into: edgeTop !== null && y < edgeTop }
}

// A long press lifts a `[data-drag]` row inside a `[data-drag-list]`; the row
// follows the pointer and lands where it is let go, above or below the list's
// `.order-edge`. Off while `drop` holds null.
export function useLiftDrag(drop: RefObject<((d: Drop) => void) | null>) {
  useEffect(() => {
    let timer = 0
    let press: { el: HTMLElement; list: HTMLElement; x: number; y: number } | null = null
    let lifted: { el: HTMLElement; list: HTMLElement; from: number; y: number } | null = null
    const rowsOf = (list: HTMLElement) => [...list.querySelectorAll<HTMLElement>('[data-drag]')]

    const settle = () => {
      if (!lifted) return
      lifted.el.style.transform = ''
      delete lifted.el.dataset.lifted
      delete lifted.list.dataset.dragging
      lifted = null
    }
    const lift = () => {
      if (!press || !drop.current) return
      const from = rowsOf(press.list).indexOf(press.el)
      if (from === -1) return
      lifted = { el: press.el, list: press.list, from, y: press.y }
      press.el.dataset.lifted = ''
      press.list.dataset.dragging = ''
      navigator.vibrate?.(10)
    }
    const down = (e: PointerEvent) => {
      if (e.button !== 0 || !drop.current) return
      const target = e.target as HTMLElement
      if (target.closest('button, input, a, [role="slider"], .task-steps')) return
      const el = target.closest<HTMLElement>('[data-drag]')
      const list = el?.closest<HTMLElement>('[data-drag-list]')
      if (!el || !list) return
      press = { el, list, x: e.clientX, y: e.clientY }
      timer = window.setTimeout(lift, HOLD_MS)
    }
    const move = (e: PointerEvent) => {
      if (press && !lifted && Math.hypot(e.clientX - press.x, e.clientY - press.y) > SLOP_PX) {
        window.clearTimeout(timer)
        press = null
      }
      if (!lifted) return
      e.preventDefault()
      lifted.el.style.transform = `translateY(${e.clientY - lifted.y}px)`
    }
    const up = (e: PointerEvent) => {
      window.clearTimeout(timer)
      press = null
      if (!lifted) return
      const { el, list, from, y } = lifted
      const others = rowsOf(list)
        .filter((r) => r !== el)
        .map((r) => r.getBoundingClientRect())
      const edge = list.querySelector<HTMLElement>('.order-edge')
      const land = landing(y, e.clientY, others, edge ? edge.getBoundingClientRect().top : null)
      settle()
      if (land) drop.current?.({ from, ...land })
    }
    const cancel = () => {
      window.clearTimeout(timer)
      press = null
      settle()
    }
    const still = (e: TouchEvent) => {
      if (lifted) e.preventDefault()
    }
    const menu = (e: Event) => {
      if (press || lifted) e.preventDefault()
    }
    document.addEventListener('pointerdown', down)
    document.addEventListener('pointermove', move, { passive: false })
    document.addEventListener('pointerup', up)
    document.addEventListener('pointercancel', cancel)
    document.addEventListener('touchmove', still, { passive: false })
    document.addEventListener('contextmenu', menu)
    return () => {
      window.clearTimeout(timer)
      document.removeEventListener('pointerdown', down)
      document.removeEventListener('pointermove', move)
      document.removeEventListener('pointerup', up)
      document.removeEventListener('pointercancel', cancel)
      document.removeEventListener('touchmove', still)
      document.removeEventListener('contextmenu', menu)
    }
  }, [drop])
}
