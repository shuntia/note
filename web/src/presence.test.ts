import { afterEach, beforeEach, expect, test, vi } from 'vitest'
import { PRESENCE_EVERY_MS, startPresence } from './presence'

let target: EventTarget
let visible: boolean
let ping: ReturnType<typeof vi.fn>
let stop: () => void

beforeEach(() => {
  vi.useFakeTimers()
  target = new EventTarget()
  visible = true
  ping = vi.fn()
  stop = startPresence({ ping, target, visible: () => visible, now: () => Date.now() })
})

afterEach(() => {
  stop()
  vi.useRealTimers()
})

const touch = () => target.dispatchEvent(new Event('pointerdown'))

test('the first touch pings at once and the rest of the minute folds into one more', () => {
  touch()
  expect(ping).toHaveBeenCalledTimes(1)
  vi.advanceTimersByTime(10_000)
  touch()
  touch()
  expect(ping).toHaveBeenCalledTimes(1)
  vi.advanceTimersByTime(PRESENCE_EVERY_MS - 10_000)
  expect(ping).toHaveBeenCalledTimes(2)
})

test('an untouched page stays silent', () => {
  vi.advanceTimersByTime(PRESENCE_EVERY_MS * 5)
  expect(ping).not.toHaveBeenCalled()
})

test('a hidden page never pings', () => {
  visible = false
  touch()
  expect(ping).not.toHaveBeenCalled()
  visible = true
  touch()
  vi.advanceTimersByTime(1000)
  touch()
  visible = false
  vi.advanceTimersByTime(PRESENCE_EVERY_MS)
  expect(ping).toHaveBeenCalledTimes(1)
})

test('stopping drops the listeners and the waiting ping', () => {
  touch()
  vi.advanceTimersByTime(1000)
  touch()
  stop()
  vi.advanceTimersByTime(PRESENCE_EVERY_MS)
  touch()
  expect(ping).toHaveBeenCalledTimes(1)
})
