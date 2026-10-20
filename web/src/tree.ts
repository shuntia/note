import type { TaskNode, TaskUrgency } from './types'

export const liveChildren = (n: TaskNode) => n.children.filter((c) => c.state !== 'dropped')

/** The node and every descendant, depth first, the node itself first. */
export function subtree(node: TaskNode): TaskNode[] {
  return [node, ...node.children.flatMap(subtree)]
}

export const holds = (node: TaskNode, id: number): boolean =>
  node.id === id || node.children.some((c) => holds(c, id))

export function findNode(nodes: TaskNode[], id: number): TaskNode | null {
  for (const n of nodes) {
    if (n.id === id) return n
    const found = findNode(n.children, id)
    if (found) return found
  }
  return null
}

/** The nodes from a root down to `id`, both included; empty when no tree holds it. */
export function pathTo(nodes: TaskNode[], id: number): TaskNode[] {
  for (const n of nodes) {
    if (n.id === id) return [n]
    const below = pathTo(n.children, id)
    if (below.length > 0) return [n, ...below]
  }
  return []
}

/** Depth first, skipping dropped branches; a node without live children is its own only leaf. */
export function leaves(node: TaskNode): TaskNode[] {
  const kids = liveChildren(node)
  return kids.length === 0 ? [node] : kids.flatMap(leaves)
}

/** Every descendant before the node it hangs from. */
export function postOrder(node: TaskNode): TaskNode[] {
  return [...node.children.flatMap(postOrder), node]
}

export const isOpen = (n: TaskNode) => n.state === 'open' || n.state === 'in_progress'

export function firstOpenLeaf(node: TaskNode): TaskNode | null {
  return leaves(node).find(isOpen) ?? null
}

/** The trees with `id` replaced by what `fn` makes of it; untouched branches keep their identity. */
export function mapNode(nodes: TaskNode[], id: number, fn: (n: TaskNode) => TaskNode): TaskNode[] {
  let changed = false
  const out = nodes.map((n) => {
    if (n.id === id) {
      changed = true
      return fn(n)
    }
    if (!holds(n, id)) return n
    changed = true
    return { ...n, children: mapNode(n.children, id, fn) }
  })
  return changed ? out : nodes
}

/** `fn` applied to the node and each of its descendants. */
export function mapSubtree(node: TaskNode, fn: (n: TaskNode) => TaskNode): TaskNode {
  return { ...fn(node), children: node.children.map((c) => mapSubtree(c, fn)) }
}

const RANK: Record<TaskUrgency, number> = { low: 0, normal: 1, high: 2 }

/** The node with its own urgency set to `urgency` and every effective urgency below it read again. */
export function withUrgency(node: TaskNode, urgency: TaskUrgency, above: TaskUrgency = 'low'): TaskNode {
  const relevel = (n: TaskNode, over: TaskUrgency): TaskNode => {
    const effective = RANK[n.urgency] >= RANK[over] ? n.urgency : over
    return { ...n, effective_urgency: effective, children: n.children.map((c) => relevel(c, effective)) }
  }
  return relevel({ ...node, urgency }, above)
}

/**
 * Finishing `id` as the server cascades it: its unfinished descendants finish too,
 * and each ancestor whose live children are then all done follows. `changed`
 * holds the prior nodes in the order an undo puts them back: descendants first.
 */
export function finish(
  roots: TaskNode[],
  id: number,
  put: (n: TaskNode) => TaskNode,
): { roots: TaskNode[]; changed: TaskNode[] } {
  const path = pathTo(roots, id)
  if (path.length === 0) return { roots, changed: [] }
  const target = path[path.length - 1]
  const changed = postOrder(target).filter(isOpen)
  const done = (n: TaskNode): TaskNode =>
    mapSubtree(n, (x) => (isOpen(x) ? put(x) : x))
  let next = mapNode(roots, id, done)
  for (const above of path.slice(0, -1).reverse()) {
    const now = findNode(next, above.id)
    if (!now || !isOpen(now) || !liveChildren(now).every((c) => c.state === 'done')) break
    changed.push(above)
    next = mapNode(next, above.id, (n) => ({ ...put(n), children: n.children }))
  }
  return { roots: next, changed }
}

/** Duration-weighted like the server derives a parent's: an unsized node weighs its siblings' mean. */
export function progressOf(node: TaskNode): number {
  if (node.state === 'done') return 100
  const kids = liveChildren(node)
  if (kids.length === 0) return node.progress
  const sized = kids.flatMap((k) => (k.duration_min === null ? [] : [k.duration_min]))
  const mean = sized.length > 0 ? sized.reduce((a, b) => a + b, 0) / sized.length : 1
  let sum = 0
  let weight = 0
  for (const k of kids) {
    const w = k.duration_min ?? mean
    sum += w * progressOf(k)
    weight += w
  }
  return weight === 0 ? 0 : Math.round(sum / weight)
}
