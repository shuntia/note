import { expect, test } from 'vitest'
import { Swipe } from './swipe'

test('a swipe swallows the click its release fires', () => {
  const s = new Swipe()
  s.down(100)
  expect(s.up(200)).toBe(true)
  expect(s.tap()).toBe(false)
  expect(s.tap()).toBe(true)
})

test('a swipe released outside the circle leaves the next keyboard tap alone', () => {
  const s = new Swipe()
  s.down(100)
  expect(s.up(200)).toBe(true)
  s.settle()
  expect(s.tap()).toBe(true)
})

test('a short drag is no swipe', () => {
  const s = new Swipe()
  s.down(100)
  expect(s.up(150)).toBe(false)
  expect(s.tap()).toBe(true)
})

test('a cancelled pointer starts no swipe', () => {
  const s = new Swipe()
  s.down(100)
  s.settle()
  expect(s.up(300)).toBe(false)
})
