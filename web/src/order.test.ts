import { describe, expect, test } from 'vitest'
import { dropped, withOrder } from './order'
import type { TaskNode } from './types'

const node = (id: number, steps: number[] = []) =>
  ({ id, children: steps.map((s) => ({ id: s })) }) as unknown as TaskNode

describe('withOrder', () => {
  test('ordered tasks lead, once each, and the rest keep their order', () => {
    const [a, b, c, d] = [node(1), node(2, [21, 22]), node(3), node(4)]
    const { rows, ordered } = withOrder([a, c], [a, b, c, d], [22, 3, 21])
    expect(rows.map((n) => n.id)).toEqual([2, 3, 1])
    expect(ordered).toBe(2)
  })
  test('an id no live task holds is passed over', () => {
    expect(withOrder([node(1)], [node(1)], [99]).ordered).toBe(0)
  })
})

describe('dropped', () => {
  const rows = [node(1), node(2), node(3), node(4), node(5)]
  const order = [1, 2, 3]

  test('a row dragged up from below the edge joins the order there', () => {
    expect(dropped(order, rows, 3, 3, 1, true)).toEqual([1, 4, 2, 3])
  })
  test('a row dragged below the edge leaves the order', () => {
    expect(dropped(order, rows, 3, 0, 4, false)).toEqual([2, 3])
  })
  test('a row moved within the order keeps the rest in place', () => {
    expect(dropped(order, rows, 3, 2, 0, true)).toEqual([3, 1, 2])
  })
  test('the first row below the edge can be dropped just above it', () => {
    expect(dropped(order, rows, 3, 3, 3, true)).toEqual([1, 2, 3, 4])
  })
  test('letting_go_where_it_started_changes_nothing', () => {
    expect(dropped(order, rows, 3, 3, 3, false)).toEqual(order)
    expect(dropped(order, rows, 3, 1, 1, true)).toEqual(order)
  })
  test('a task held through its steps keeps its step ids', () => {
    const held = [node(2, [21, 22]), node(1)]
    expect(dropped([22, 1], held, 2, 0, 0, true)).toEqual([22, 1])
    expect(dropped([22, 1], held, 2, 0, 1, true)).toEqual([1, 22])
  })
  test('with an empty order a row dropped above the edge starts one', () => {
    expect(dropped([], rows, 0, 2, 0, true)).toEqual([3])
  })
})
