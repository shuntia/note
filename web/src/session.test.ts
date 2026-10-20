import { expect, test } from 'vitest'
import { quickStop, widgetClock, type FocusSession } from './session'

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

test('the widget clock times a pomodoro by its phase and a plain session by its plan', () => {
  const pomodoro = started('2026-09-30T12:00:00Z', {
    mode: 'pomodoro', phase: 'break', work_min: 25, break_min: 5,
    phase_started_at: '2026-09-30T12:25:00Z', phase_paused_ms: 1000, planned_min: null,
  })
  expect(widgetClock(pomodoro, 'elapsed')).toEqual({
    start: at('2026-09-30T12:25:01Z'), total: 300, pomodoro: true, onBreak: true, prefer: 'elapsed', pausedAt: null,
  })
  const plain = started('2026-09-30T12:00:00Z', { mode: 'single', planned_min: 30, paused_ms: 2000, paused_at: '2026-09-30T12:10:00Z' })
  expect(widgetClock(plain, 'remaining')).toEqual({
    start: at('2026-09-30T12:00:02Z'), total: 1800, pomodoro: false, onBreak: false, prefer: 'remaining', pausedAt: at('2026-09-30T12:10:00Z'),
  })
})
