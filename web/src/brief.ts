import { minutesOf } from './dayline'
import type { Debrief, Review } from './types'

export const BRIEF_CLEAR_MIN = 90

const READ_KEY = 'note.briefRead'
const READ_KEEP = 14

export type Brief = { kind: 'letter' | 'review'; key: string; content: string }

/** Local YYYY-MM-DD of the Monday starting `day`'s week. */
export function mondayOf(day: Date): string {
  const at = new Date(day)
  at.setHours(0, 0, 0, 0)
  at.setDate(at.getDate() - ((at.getDay() + 6) % 7))
  const month = `${at.getMonth() + 1}`.padStart(2, '0')
  return `${at.getFullYear()}-${month}-${`${at.getDate()}`.padStart(2, '0')}`
}

// A letter older than last week belongs to a week nobody is still living in.
export function stillCurrent(week: string, today: Date): boolean {
  const previous = new Date(today)
  previous.setDate(previous.getDate() - 7)
  return week === mondayOf(today) || week === mondayOf(previous)
}

/**
 * The morning is open before `until` while nothing is on for the next
 * `BRIEF_CLEAR_MIN` minutes; `busyIn` is the minutes to the next thing, 0 while one
 * runs, null when nothing is left today.
 */
export function morningOpen(now: number, until: string, busyIn: number | null): boolean {
  return now < minutesOf(until) && (busyIn === null || busyIn > BRIEF_CLEAR_MIN)
}

/** The morning's letter first, then the week's review; whichever is unread. */
export function pickBrief(
  letter: Debrief | null,
  review: Review | null,
  date: string,
  today: Date,
  read: ReadonlySet<string>,
): Brief | null {
  if (letter && letter.date === date && !read.has(`letter:${letter.date}`)) {
    return { kind: 'letter', key: `letter:${letter.date}`, content: letter.content }
  }
  if (review && stillCurrent(review.week_start, today) && !read.has(`review:${review.week_start}`)) {
    return { kind: 'review', key: `review:${review.week_start}`, content: review.content }
  }
  return null
}

/** Minutes until the soonest of the given spans starts, 0 when one is running. */
export function busyIn(spans: { start: string; end: string }[], now: number): number | null {
  let soonest: number | null = null
  for (const { start, end } of spans) {
    if (minutesOf(end) <= now) continue
    const wait = Math.max(0, minutesOf(start) - now)
    soonest = soonest === null ? wait : Math.min(soonest, wait)
  }
  return soonest
}

export function readBriefs(): Set<string> {
  try {
    const raw = localStorage.getItem(READ_KEY)
    return new Set(raw ? (JSON.parse(raw) as string[]) : [])
  } catch {
    return new Set()
  }
}

export function markBriefRead(key: string): Set<string> {
  const read = [...readBriefs().add(key)].slice(-READ_KEEP)
  try {
    localStorage.setItem(READ_KEY, JSON.stringify(read))
  } catch {
    // storage blocked; the brief stays read until the page reloads
  }
  return new Set(read)
}
