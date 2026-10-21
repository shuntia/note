import type { Goal, Task } from './types'

type Dated = Pick<Task, 'due_at' | 'due_on'>

// What a due editor sends: a whole day, a moment, or nothing.
export type DueBody = { due_on: string } | { due_at: string } | { due_on: null }

const pad = (n: number) => String(n).padStart(2, '0')

export const localDay = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`

// `date` and `time` as a date and a time input hold them; an empty time is the
// whole day. `null` when the date does not read.
export function dueBody(date: string, time: string): DueBody | null {
  if (date === '') return { due_on: null }
  const [y, m, d] = date.split('-').map(Number)
  if (!y || !m || !d) return null
  if (time === '') return { due_on: date }
  const [h, min] = time.split(':').map(Number)
  if (h === undefined || min === undefined || Number.isNaN(h) || Number.isNaN(min)) return null
  return { due_at: new Date(y, m - 1, d, h, min).toISOString() }
}

export function dueDraft(x: Dated): { date: string; time: string } {
  if (x.due_on !== null) return { date: x.due_on, time: '' }
  const at = x.due_at === null ? null : new Date(x.due_at)
  if (at === null || Number.isNaN(at.getTime())) return { date: '', time: '' }
  return { date: localDay(at), time: `${pad(at.getHours())}:${pad(at.getMinutes())}` }
}

// The day it falls on, at local midnight, and the moment when it has one.
export function dueParts(x: Dated): { day: Date; at: Date | null } | null {
  if (x.due_on !== null) {
    const [y, m, d] = x.due_on.split('-').map(Number)
    return y && m && d ? { day: new Date(y, m - 1, d), at: null } : null
  }
  if (x.due_at === null) return null
  const at = new Date(x.due_at)
  if (Number.isNaN(at.getTime())) return null
  return { day: new Date(at.getFullYear(), at.getMonth(), at.getDate()), at }
}

export const goalDated = (g: Goal): Dated => ({ due_at: g.due_at, due_on: g.due_on })
