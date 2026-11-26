import { expect, test } from 'vitest'
import { dayLineEnd, workDate, workMinutes } from './workday'

const at = (iso: string) => new Date(iso)

test('the small hours belong to the day before until the day ends', () => {
  expect(workDate(at('2026-11-25T01:30:00'), '03:00')).toBe('2026-11-24')
  expect(workDate(at('2026-11-25T03:00:00'), '03:00')).toBe('2026-11-25')
  expect(workDate(at('2026-11-25T01:30:00'), '00:00')).toBe('2026-11-25')
  expect(workDate(at('2026-03-01T00:10:00'), '02:00')).toBe('2026-02-28')
})

test('the clock reads past 24:00 while the day runs on', () => {
  expect(workMinutes(at('2026-11-25T01:30:00'), '03:00')).toBe(25 * 60 + 30)
  expect(workMinutes(at('2026-11-25T13:15:00'), '03:00')).toBe(13 * 60 + 15)
  expect(workMinutes(at('2026-11-25T01:30:00'), '00:00')).toBe(90)
  expect(dayLineEnd('00:00')).toBe(24 * 60)
  expect(dayLineEnd('03:00')).toBe(27 * 60)
})
