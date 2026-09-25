import { expect, test } from 'vitest'
import { waveDelays } from './wave'

const r = (top: number, height = 36): DOMRectReadOnly =>
  ({ top, bottom: top + height, left: 0, right: 100, height, width: 100, x: 0, y: top, toJSON() {} }) as DOMRectReadOnly

test('rows delay by their top edge, capped at the total', () => {
  expect(waveDelays([r(0), r(100), r(200)])).toEqual([0, 0.12, 0.24])
})

test('equal tops all start at once', () => {
  expect(waveDelays([r(50), r(50), r(50)])).toEqual([0, 0, 0])
})

test('rows outside the box are left out', () => {
  const box = r(0, 300)
  expect(waveDelays([r(-100), r(10), r(290), r(400)], box)).toEqual([null, 0, 0.24, null])
})

test('two columns rise together', () => {
  const left = [r(0), r(40)]
  const right = [r(0), r(40)].map((x) => ({ ...x, left: 500, right: 600 }) as DOMRectReadOnly)
  expect(waveDelays([...left, ...right])).toEqual([0, 0.24, 0, 0.24])
})
