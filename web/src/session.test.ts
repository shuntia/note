import { expect, test } from 'vitest'
import { quickStop, type FocusSession } from './session'

const started = (iso: string, o: Partial<FocusSession> = {}): FocusSession =>
  ({ id: 1, title: 'Read', started_at: iso, paused_at: null, paused_ms: 0, ...o }) as unknown as FocusSession
const at = (iso: string) => Date.parse(iso)

test('a stop inside the first minute discards the session', () => {
  expect(quickStop(started('2026-09-30T12:00:00Z'), at('2026-09-30T12:00:59Z'))).toBe(true)
  expect(quickStop(started('2026-09-30T12:00:00Z'), at('2026-09-30T12:01:00Z'))).toBe(false)
})

test('the minute runs on the wall clock, paused or not', () => {
  const s = started('2026-09-30T12:00:00Z', { paused_at: '2026-09-30T12:00:20Z' })
  expect(quickStop(s, at('2026-09-30T12:03:00Z'))).toBe(false)
})

test('an unreadable start never discards', () => {
  expect(quickStop(started('not a time'), at('2026-09-30T12:00:10Z'))).toBe(false)
})
