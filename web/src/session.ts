import type { WorkSession } from './types'

const SESSION_KEY = 'note.nowSession'

// The session as the server holds it; the client only ever paints it.
export type FocusSession = WorkSession

export type CounterMode = 'elapsed' | 'remaining'

function isSession(v: unknown): v is FocusSession {
  if (typeof v !== 'object' || v === null) return false
  const s = v as Record<string, unknown>
  return (
    typeof s.id === 'number' &&
    typeof s.title === 'string' &&
    typeof s.started_at === 'string' &&
    typeof s.paused_ms === 'number'
  )
}

// The last session seen, for the first paint only: `/api/sessions/open` is the truth.
export function readSession(): FocusSession | null {
  try {
    const raw = localStorage.getItem(SESSION_KEY)
    if (!raw) return null
    const parsed: unknown = JSON.parse(raw)
    return isSession(parsed) ? parsed : null
  } catch {
    return null
  }
}

export function writeSession(s: FocusSession | null) {
  try {
    if (s) localStorage.setItem(SESSION_KEY, JSON.stringify(s))
    else localStorage.removeItem(SESSION_KEY)
  } catch {
    // storage blocked; the session still holds for this tab
  }
}

// A session ended while its undo window is open: a refetch must not bring it back.
let ending: number | null = null

export function markEnding(id: number | null) {
  ending = id
}

export function stillEnding(id: number): boolean {
  return ending === id
}

function at(iso: string | null): number | null {
  if (!iso) return null
  const t = Date.parse(iso)
  return Number.isNaN(t) ? null : t
}

export function isPaused(s: FocusSession): boolean {
  return s.paused_at !== null
}

export function pausedAt(s: FocusSession): number | null {
  return at(s.paused_at)
}

// Paused time is folded into the start, so one clock reading is the whole answer:
// a paused session freezes at `paused_at` and resuming carries on from there.
export function effectiveStart(s: FocusSession): number {
  return (at(s.started_at) ?? Date.now()) + s.paused_ms
}

export function phaseStart(s: FocusSession): number {
  return (at(s.phase_started_at) ?? at(s.started_at) ?? Date.now()) + s.phase_paused_ms
}

const since = (s: FocusSession, start: number) =>
  Math.max(0, Math.floor(((pausedAt(s) ?? Date.now()) - start) / 1000))

export function elapsedSec(s: FocusSession): number {
  return since(s, effectiveStart(s))
}

// The server discards a session ended within a minute of its start, by the wall clock.
export function quickStop(s: FocusSession, now: number): boolean {
  const start = at(s.started_at)
  return start !== null && now - start < 60_000
}

export function phaseElapsedSec(s: FocusSession): number {
  return since(s, phaseStart(s))
}

export function plannedSec(s: FocusSession): number | null {
  return s.planned_min === null ? null : s.planned_min * 60
}

// How long this round runs; null wherever the session is not a pomodoro one.
export function phaseLengthSec(s: FocusSession): number | null {
  if (s.mode !== 'pomodoro') return null
  const min = s.phase === 'break' ? s.break_min : s.work_min
  return min === null ? null : min * 60
}

export function phaseRemainingSec(s: FocusSession): number {
  const length = phaseLengthSec(s)
  return length === null ? 0 : Math.max(0, length - phaseElapsedSec(s))
}
