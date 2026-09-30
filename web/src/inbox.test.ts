import { describe, expect, test } from 'vitest'
import { arrivalLabel, mergePage, outcomeLabel, refreshSettled, REFRESH_WINDOW_MS } from './inbox'

describe('refreshSettled', () => {
  const pressed = '2026-09-30T10:00:00.000000Z'

  test('keeps spinning while nothing newer has arrived inside the window', () => {
    expect(refreshSettled(pressed, null, 0)).toBe(false)
    expect(refreshSettled(pressed, '2026-09-30T09:59:59.999999Z', 30_000)).toBe(false)
    expect(refreshSettled(pressed, pressed, REFRESH_WINDOW_MS - 1)).toBe(false)
  })

  test('stops at once when an item arrives after the press', () => {
    expect(refreshSettled(pressed, '2026-09-30T10:00:00.000001Z', 3_000)).toBe(true)
  })

  test('stops when the window closes with nothing new', () => {
    expect(refreshSettled(pressed, null, REFRESH_WINDOW_MS)).toBe(true)
    expect(refreshSettled(pressed, '2026-09-29T00:00:00.000000Z', REFRESH_WINDOW_MS + 5_000)).toBe(true)
  })
})

describe('arrivalLabel', () => {
  const now = new Date(2026, 8, 30, 18, 0)

  test('today reads as a time', () => {
    const at = new Date(2026, 8, 30, 9, 5)
    expect(arrivalLabel(at.toISOString(), now)).toBe(
      at.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' }),
    )
  })

  test('an earlier day reads as a date, with the year only when it differs', () => {
    const sameYear = new Date(2026, 8, 28, 9, 5)
    expect(arrivalLabel(sameYear.toISOString(), now)).toBe(
      sameYear.toLocaleDateString(undefined, { month: 'short', day: 'numeric' }),
    )
    const lastYear = new Date(2025, 11, 31, 9, 5)
    expect(arrivalLabel(lastYear.toISOString(), now)).toBe(
      lastYear.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' }),
    )
  })

  test('an unreadable time reads as nothing', () => {
    expect(arrivalLabel('soon', now)).toBe('')
  })
})

test('an undecided item reads as pending', () => {
  expect(outcomeLabel(null)).toBe('pending')
  expect(outcomeLabel('remembered')).toBe('remembered')
})

test('an older page appends without repeating a row already shown', () => {
  const merged = mergePage([{ id: 3 }, { id: 2 }], [{ id: 2 }, { id: 1 }])
  expect(merged.map((r) => r.id)).toEqual([3, 2, 1])
})
