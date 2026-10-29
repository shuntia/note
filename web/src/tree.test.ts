import { describe, expect, test } from 'vitest'
import { findNode, finish, firstOpenLeaf, holds, leaves, mapNode, mapSubtree, pathTo, progressOf, subtree, unpinAbove, weighted, withUrgency } from './tree'
import type { TaskNode, TaskState } from './types'

const n = (id: number, children: TaskNode[] = [], state: TaskState = 'open') =>
  ({ id, state, children }) as unknown as TaskNode

// 1 ─┬─ 2 ─┬─ 4 (done)
//    │     └─ 5 ── 7
//    └─ 3
const tree = () => n(1, [n(2, [n(4, [], 'done'), n(5, [n(7)])]), n(3)])
const ids = (ns: TaskNode[]) => ns.map((x) => x.id)

describe('tree', () => {
  test('subtree runs depth first from the node', () => {
    expect(ids(subtree(tree()))).toEqual([1, 2, 4, 5, 7, 3])
  })
  test('holds and findNode reach any depth', () => {
    expect(holds(tree(), 7)).toBe(true)
    expect(holds(tree(), 9)).toBe(false)
    expect(findNode([n(9), tree()], 7)?.id).toBe(7)
    expect(findNode([tree()], 99)).toBeNull()
  })
  test('pathTo runs from the root to the node', () => {
    expect(ids(pathTo([n(9), tree()], 7))).toEqual([1, 2, 5, 7])
    expect(pathTo([tree()], 99)).toEqual([])
  })
  test('leaves are the childless nodes in order, and a lone node is its own', () => {
    expect(ids(leaves(tree()))).toEqual([4, 7, 3])
    expect(ids(leaves(n(8)))).toEqual([8])
  })
  test('the first open leaf skips finished ones', () => {
    expect(firstOpenLeaf(tree())?.id).toBe(7)
    expect(firstOpenLeaf(n(1, [n(2, [], 'done')]))).toBeNull()
  })
  test('mapNode changes one node and keeps untouched branches', () => {
    const roots = [tree(), n(9)]
    const out = mapNode(roots, 7, (x) => ({ ...x, state: 'done' }))
    expect(findNode(out, 7)?.state).toBe('done')
    expect(out[1]).toBe(roots[1])
    expect(out[0].children[1]).toBe(roots[0].children[1])
    expect(mapNode(roots, 99, (x) => x)).toBe(roots)
  })
  test('mapSubtree reaches every descendant', () => {
    const out = mapSubtree(tree().children[0], (x) => ({ ...x, state: 'dropped' }))
    expect(subtree(out).every((x) => x.state === 'dropped')).toBe(true)
  })
})

const full = (id: number, o: Partial<TaskNode> = {}, children: TaskNode[] = []) =>
  ({ id, state: 'open', progress: 0, duration_min: null, urgency: 'normal', effective_urgency: 'normal', ...o, children }) as unknown as TaskNode

describe('finish', () => {
  const put = (x: TaskNode) => ({ ...x, state: 'done' as TaskState })

  test('a parent takes its open descendants with it, and they undo first', () => {
    const roots = [full(1, {}, [full(2, {}, [full(4), full(5, { state: 'done' })]), full(3)])]
    const { roots: out, changed } = finish(roots, 2, put)
    expect(ids(changed)).toEqual([4, 2])
    expect(subtree(out[0]).map((x) => x.state)).toEqual(['open', 'done', 'done', 'done', 'open'])
  })
  test('the last open leaf finishes each ancestor it completes', () => {
    const roots = [full(1, {}, [full(2, {}, [full(4)]), full(3, { state: 'done' })])]
    const { roots: out, changed } = finish(roots, 4, put)
    expect(ids(changed)).toEqual([4, 2, 1])
    expect(out[0].state).toBe('done')
  })
  test('a dropped sibling does not hold its parent open', () => {
    const roots = [full(1, {}, [full(2), full(3, { state: 'dropped' })])]
    expect(ids(finish(roots, 2, put).changed)).toEqual([2, 1])
  })
})

describe('urgency and progress', () => {
  test('an urgency reaches every effective urgency below it, never below its own', () => {
    const node = full(1, {}, [full(2, { urgency: 'high', effective_urgency: 'high' }, [full(3)]), full(4)])
    const low = withUrgency(node, 'low')
    expect(subtree(low).map((x) => x.effective_urgency)).toEqual(['low', 'high', 'high', 'normal'])
    expect(subtree(withUrgency(node, 'high')).every((x) => x.effective_urgency === 'high')).toBe(true)
  })
  test('progress weighs each live child by its length, recursively', () => {
    const node = full(1, {}, [
      full(2, { duration_min: 30 }, [full(5, { duration_min: 10, state: 'done' }), full(6, { duration_min: 20 })]),
      full(3, { duration_min: 10, progress: 50 }),
      full(4, { state: 'dropped' }),
    ])
    expect(progressOf(node)).toBe(Math.round((30 * 33 + 10 * 50) / 40))
  })
  test('a hand-set parent stands in for its steps, and only the parents above a moved step give way', () => {
    const node = full(1, {}, [
      full(2, { duration_min: 30, progress: 80, progress_set: true }, [full(5, { duration_min: 30 })]),
      full(3, { duration_min: 10, progress: 0 }, [full(6, { duration_min: 10 })]),
    ])
    expect(progressOf(node)).toBe(60)
    const [back] = unpinAbove([{ ...node, progress_set: true }], 5)
    expect(back.progress_set).toBe(false)
    expect(back.children[0].progress_set).toBe(false)
    expect(progressOf(back)).toBe(0)
  })
  test('an unsized part weighs the mean of the sized ones', () => {
    expect(weighted([{ min: 60, pct: 100 }, { min: 20, pct: 0 }, { min: null, pct: 0 }])).toBe(50)
    expect(weighted([])).toBe(0)
  })
})
