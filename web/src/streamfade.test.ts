import { expect, test } from 'vitest'
import { FADE_MS, pieces, schedule, words } from './streamfade'

test('words rejoin to the text they came from', () => {
  for (const text of ['a b ', '  ', 'one\n\ntwo', ' lead', 'x', '']) expect(words(text).join('')).toBe(text)
})

test('a chunk becomes contiguous word segments staggered from now', () => {
  const segs = schedule('hello there world', 10, 1000)
  expect(segs.map((s) => [s.from, s.to])).toEqual([[10, 15], [15, 21], [21, 27]])
  expect(segs[0].start).toBe(1000)
  expect(segs[1].start).toBeGreaterThan(segs[0].start)
})

test('a large burst is compressed into a short spread and a bounded span count', () => {
  const text = Array.from({ length: 2000 }, (_, i) => `w${i}`).join(' ')
  const segs = schedule(text, 0, 0)
  expect(segs.length).toBeLessThanOrEqual(40)
  expect(segs.at(-1)!.to).toBe(text.length)
  expect(segs.at(-1)!.start).toBeLessThanOrEqual(200)
})

test('a burst queues behind the one still staggering in, but never far behind now', () => {
  expect(schedule('next', 0, 100, 110)[0].start).toBeGreaterThan(110)
  expect(schedule('next', 0, 100, 5000)[0].start).toBeLessThanOrEqual(100 + FADE_MS)
  expect(schedule('next', 0, 100, 0)[0].start).toBe(100)
})

test('segments split across text nodes, each node last piece first', () => {
  const got = pieces([4, 6], [{ from: 2, to: 7, start: 50 }, { from: 7, to: 10, start: 60 }], 40)
  expect(got).toEqual([
    { node: 0, from: 2, to: 4, delay: 10 },
    { node: 1, from: 3, to: 6, delay: 20 },
    { node: 1, from: 0, to: 3, delay: 10 },
  ])
})
