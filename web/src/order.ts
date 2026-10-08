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
