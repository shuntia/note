const SESSION_KEY = 'note.nowSession'

// One focus session. `taskId` names the task being worked — a step, when the
// task was split — while `title` stays the top-level name the user recognises.
export type FocusSession = {
  taskId: number | null
  eventId: number | null
  title: string
  notes: string
  stepIndex: number | null
  stepCount: number | null
  stepName: string | null
  durationSec: number | null
  startedAt: number
  pausedAt: number | null
  pausedMs: number
}

export type CounterMode = 'elapsed' | 'remaining'

function isSession(v: unknown): v is FocusSession {
  if (typeof v !== 'object' || v === null) return false
  const s = v as Record<string, unknown>
  return (
    typeof s.title === 'string' &&
    typeof s.startedAt === 'number' &&
    typeof s.pausedMs === 'number'
  )
}

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

// Paused time never counts, so the counter can read one clock and still be right.
export function effectiveStart(s: FocusSession): number {
  return s.startedAt + s.pausedMs
}

export function elapsedSec(s: FocusSession): number {
  const at = s.pausedAt ?? Date.now()
  return Math.max(0, Math.floor((at - effectiveStart(s)) / 1000))
}
