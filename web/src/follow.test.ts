import { expect, test } from 'vitest'
import { follows, newFollow } from './follow'

const at = (top: number, height = 2000) => ({ top, height, client: 500 })

test('content outgrowing an ease toward the end keeps the pane following', () => {
  const f = newFollow()
  let stick = follows(f, true, at(1500), 0)
  stick = follows(f, stick, at(1520, 2600), 16)
  stick = follows(f, stick, at(1600, 4000), 32)
  expect(stick).toBe(true)
})

test('content shrinking under the pane without the user does not let go', () => {
  const f = newFollow()
  follows(f, true, at(1500), 0)
  expect(follows(f, true, at(900, 1300), 5000)).toBe(true)
})

test('a wheel back up lets go, and reaching the end takes hold again', () => {
  const f = newFollow()
  follows(f, true, at(1500), 0)
  f.intentAt = 100
  let stick = follows(f, true, at(1300), 120)
  expect(stick).toBe(false)
  stick = follows(f, stick, at(1350, 3000), 2000)
  expect(stick).toBe(false)
  stick = follows(f, stick, at(2480, 3000), 2100)
  expect(stick).toBe(true)
})

test('a scrollbar drag held past the intent window still lets go', () => {
  const f = newFollow()
  follows(f, true, at(1500), 0)
  f.held = true
  expect(follows(f, true, at(1200), 9000)).toBe(false)
})

test('a scroll down by the user away from the end does not let go', () => {
  const f = newFollow()
  follows(f, true, at(100), 0)
  f.intentAt = 0
  expect(follows(f, true, at(300), 10)).toBe(true)
})
