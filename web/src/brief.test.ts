import { describe, expect, test } from 'vitest'
import { busyIn, mondayOf, morningOpen, pickBrief, stillCurrent } from './brief'

const at = (hhmm: string) => {
  const [h, m] = hhmm.split(':').map(Number)
  return h * 60 + m
}

describe('morningOpen', () => {
  test('before the end of the morning with nothing for an hour and a half', () => {
    expect(morningOpen(at('08:00'), '11:00', null)).toBe(true)
    expect(morningOpen(at('08:00'), '11:00', 91)).toBe(true)
  })
  test('closed once something is near, running, or the morning is over', () => {
    expect(morningOpen(at('08:00'), '11:00', 90)).toBe(false)
    expect(morningOpen(at('08:00'), '11:00', 0)).toBe(false)
    expect(morningOpen(at('11:00'), '11:00', null)).toBe(false)
    expect(morningOpen(at('09:59'), '10:00', null)).toBe(true)
  })
})

describe('busyIn', () => {
  test('counts to the soonest start, zero while one runs, ignoring what is over', () => {
    expect(busyIn([], at('08:00'))).toBeNull()
    expect(busyIn([{ start: '07:00', end: '07:30' }], at('08:00'))).toBeNull()
    expect(busyIn([{ start: '11:00', end: '12:00' }, { start: '09:30', end: '10:00' }], at('08:00'))).toBe(90)
    expect(busyIn([{ start: '07:30', end: '08:30' }], at('08:00'))).toBe(0)
  })
})

describe('pickBrief', () => {
  const wed = new Date(2026, 8, 30, 8)
  const letter = { date: '2026-09-30', content: 'Good morning.' }
  const review = { week_start: '2026-09-21', content: 'Last week.' }

  test("today's letter first, then this or last week's review", () => {
    expect(pickBrief(letter, review, '2026-09-30', wed, new Set())?.kind).toBe('letter')
    expect(pickBrief(letter, review, '2026-09-30', wed, new Set(['letter:2026-09-30']))?.kind).toBe('review')
    expect(pickBrief(letter, review, '2026-09-30', wed, new Set(['letter:2026-09-30', 'review:2026-09-21']))).toBeNull()
  })
  test("yesterday's letter and an old review stay away", () => {
    expect(pickBrief({ ...letter, date: '2026-09-29' }, null, '2026-09-30', wed, new Set())).toBeNull()
    expect(pickBrief(null, { ...review, week_start: '2026-09-14' }, '2026-09-30', wed, new Set())).toBeNull()
  })
})

test('weeks start on Monday', () => {
  expect(mondayOf(new Date(2026, 8, 30))).toBe('2026-09-28')
  expect(mondayOf(new Date(2026, 9, 4))).toBe('2026-09-28')
  expect(stillCurrent('2026-09-21', new Date(2026, 8, 30))).toBe(true)
})
