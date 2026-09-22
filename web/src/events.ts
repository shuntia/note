import { minutesOf } from './dayline'
import type { PlanEvent } from './types'

export type EventFacts = {
  eyebrow: 'Now' | 'Next'
  minutes: number | null
  wait: string
  span: string
}

// The fired event owns the face; failing that, the next routine or task block still ahead does.
// A trigger is Note's own moment to look again, not something the user starts,
// so it stays off the face and lives on the line and in the list.
export function nextUp(events: PlanEvent[], now: number): PlanEvent | null {
  const theirs = events.filter((ev) => ev.kind !== 'trigger')
  return (
    theirs.find((ev) => ev.status === 'fired') ??
    theirs.find(
      (ev) =>
        (ev.entry !== 'block' || ev.task) &&
        (ev.status === 'pending' || ev.status === 'snoozed') &&
        minutesOf(ev.end_wall_time ?? ev.wall_time) >= now,
    ) ??
    null
  )
}

// Up to an hour and a half a wait is counted in minutes; past it, in half hours.
function waitWords(minutes: number | null): string {
  if (minutes === null) return ''
  if (minutes <= 90) return `${minutes} min`
  return `${Math.round(minutes / 30) / 2} h`
}

// An event that has fired, or whose time has come, is now rather than in N minutes.
export function eventFacts(ev: PlanEvent, now: number): EventFacts {
  const at = minutesOf(ev.wall_time)
  const here = ev.status === 'fired' || at <= now
  const minutes = here ? null : at - now
  return {
    eyebrow: here ? 'Now' : 'Next',
    minutes,
    wait: waitWords(minutes),
    span: `${ev.wall_time} – ${ev.end_wall_time ?? ev.wall_time}`,
  }
}
