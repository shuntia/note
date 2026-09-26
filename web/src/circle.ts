import type { QueueEntry, SessionStart } from './types'

export const R = 148
export const C = 2 * Math.PI * R
export const SPAN = (C * 240) / 360

export type Pt = { x: number; y: number }
export type Segment = { start: number; len: number; frac: number }

/** Bead i of total across the ring's opening (30°..150°, SVG degrees). */
export function beadAt(i: number, total: number): Pt {
  const a = ((30 + (120 * (i + 1)) / (total + 1)) * Math.PI) / 180
  return { x: 160 + R * Math.cos(a), y: 160 + R * Math.sin(a) }
}

/** Arc segments for a task's steps: `start` in degrees, `len` in px; done 1, current `frac`, rest 0. */
export function segments(steps: number, current: number, frac: number): Segment[] {
  const gap = 5
  const seg = (240 - gap * (steps - 1)) / steps
  return Array.from({ length: steps }, (_, i) => ({
    start: 150 + i * (seg + gap),
    len: (C * seg) / 360,
    frac: i < current ? 1 : i === current ? frac : 0,
  }))
}

/** Where a horizontal release lands: one slot past 60 px, or 20 px when quick. */
export function slotAfter(index: number, dx: number, quick: boolean, count: number): number {
  const step = dx < -60 || (quick && dx < -20) ? 1 : dx > 60 || (quick && dx > 20) ? -1 : 0
  return Math.min(count - 1, Math.max(0, index + step))
}

/** The rail offset while dragging, rubber-banding by 0.3 past either end. */
export function railX(index: number, dx: number, count: number, width = 320): number {
  const edge = (index === 0 && dx > 0) || (index === count - 1 && dx < 0)
  return -index * width + (edge ? dx * 0.3 : dx)
}

type Hint = 'start' | 'session'

function hintCount(key: Hint): number {
  try {
    const n = Number(localStorage.getItem(`note.hints.${key}`))
    return Number.isFinite(n) ? n : 0
  } catch {
    return 0
  }
}

export const hintShown = (key: Hint): boolean => hintCount(key) < 3

export function hintUsed(key: Hint): void {
  try {
    localStorage.setItem(`note.hints.${key}`, String(hintCount(key) + 1))
  } catch {
    return
  }
}

/** A title that opens with the task's own category ("MIT — Draft essays") says the course twice over. */
export function withoutCategory(title: string, category: string): string {
  const prefix = `${category} — `
  if (!category || !title.startsWith(prefix)) return title
  const rest = title.slice(prefix.length).trim()
  return rest === '' ? title : rest
}

/** The session fields for a queue entry, as the Tasks view's startFocus builds them. */
export function sessionFor(entry: QueueEntry): SessionStart {
  const { task, step, planned_min } = entry
  const index = step ? task.children.findIndex((c) => c.id === step.id) : -1
  return {
    title: withoutCategory(task.title, task.category),
    task_id: task.id,
    notes: (step ?? task).notes,
    ...(index !== -1 &&
      step && {
        step_index: index + 1,
        step_count: task.children.length,
        step_name: withoutCategory(step.title, task.category),
      }),
    ...(planned_min !== null && { planned_min }),
  }
}

export const WORK_TIME: SessionStart = { title: 'Work time' }
