import type { InboxOutcome } from './types'

export const REFRESH_WINDOW_MS = 60_000
export const REFRESH_POLL_MS = 3_000
export const UP_TO_DATE_MS = 4_000

export function outcomeLabel(outcome: InboxOutcome | null): string {
  return outcome ?? 'pending'
}

// Both times are the server's fixed-width stamps, so text order is time order.
export function refreshSettled(requestedAt: string, latest: string | null, elapsedMs: number): boolean {
  return (latest !== null && latest > requestedAt) || elapsedMs >= REFRESH_WINDOW_MS
}

export function arrivalLabel(iso: string, now: Date): string {
  const at = new Date(iso)
  if (Number.isNaN(at.getTime())) return ''
  if (at.toDateString() === now.toDateString()) {
    return at.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })
  }
  return at.toLocaleDateString(undefined, {
    month: 'short',
    day: 'numeric',
    ...(at.getFullYear() === now.getFullYear() ? {} : { year: 'numeric' }),
  })
}

export function mergePage<T extends { id: number }>(current: T[], next: T[]): T[] {
  const seen = new Set(current.map((r) => r.id))
  return [...current, ...next.filter((r) => !seen.has(r.id))]
}
