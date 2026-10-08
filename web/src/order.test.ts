import { describe, expect, test } from 'vitest'
import { dropped, gapAt, landing, withOrder } from './order'
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

describe('gapAt', () => {
  const slots = [
    { top: 0, height: 40 },
    { top: 40, height: 40 },
    { top: 86, height: 1, edge: true },
    { top: 93, height: 40 },
  ]

  test('the gap opens before the first slot whose middle lies below the pointer', () => {
    expect(gapAt(50, slots)).toEqual({ at: 1, to: 1, into: true })
  })
  test('a pointer just under the edge opens the gap beneath it', () => {
    expect(gapAt(90, slots)).toEqual({ at: 3, to: 2, into: false })
  })
  test('a pointer just over the edge keeps the gap inside the order', () => {
    expect(gapAt(84, slots)).toEqual({ at: 2, to: 2, into: true })
  })
  test('past the last slot the gap opens at the end', () => {
    expect(gapAt(500, slots)).toEqual({ at: 4, to: 3, into: false })
  })
  test('without an edge nothing joins the order', () => {
    expect(gapAt(10, [{ top: 0, height: 40 }]).into).toBe(false)
  })
})

describe('landing', () => {
  const slots = [
    { top: 0, height: 40 },
    { top: 46, height: 1, edge: true },
  ]

  test('a release away from the lift lands where the middle of the row is', () => {
    expect(landing(-90, 100, slots)).toEqual({ to: 0, into: true })
    expect(landing(30, 30, slots)).toEqual({ to: 1, into: false })
  })
  test('a release where the row was lifted lands nowhere', () => {
    expect(landing(4, 100, slots)).toBeNull()
  })
})
