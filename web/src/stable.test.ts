import { expect, test } from 'vitest'
import { reuse } from './stable'

const tree = () => [
  { id: 1, title: 'a', children: [{ id: 11, title: 'a1', children: [] }] },
  { id: 2, title: 'b', children: [] },
]

test('an unchanged refetch keeps the old value whole', () => {
  const prev = tree()
  expect(reuse(prev, tree())).toBe(prev)
})

test('a change renews only the changed row and what holds it', () => {
  const prev = tree()
  const next = tree()
  next[0].children[0].title = 'a1!'
  const out = reuse(prev, next)
  expect(out).not.toBe(prev)
  expect(out).toEqual(next)
  expect(out[0]).not.toBe(prev[0])
  expect(out[0].children[0]).not.toBe(prev[0].children[0])
  expect(out[1]).toBe(prev[1])
})

test('rows are matched by id, so a reorder keeps each row', () => {
  const prev = tree()
  const out = reuse(prev, [...tree()].reverse())
  expect(out).not.toBe(prev)
  expect(out[0]).toBe(prev[1])
  expect(out[1]).toBe(prev[0])
})

test('an added or removed field counts as a change', () => {
  const prev: Record<string, unknown> = { id: 1, a: 1 }
  expect(reuse(prev, { id: 1 })).toEqual({ id: 1 })
  expect(reuse(prev, { id: 1, a: 1, b: undefined })).not.toBe(prev)
})

test('plain values and arrays without ids', () => {
  const ids = [3, 1, 2]
  expect(reuse(ids, [3, 1, 2])).toBe(ids)
  expect(reuse(ids, [3, 2])).toEqual([3, 2])
  expect(reuse<number[] | null>(null, [1])).toEqual([1])
})
