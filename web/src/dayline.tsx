import { eventLabel } from './receipts'
import type { PlanEvent } from './types'

const START = 6 * 60
const END = 24 * 60

export function minutesOf(wall: string): number {
  const [h, m] = wall.split(':')
  return Number(h) * 60 + Number(m)
}

const pct = (mins: number) => `${((Math.min(END, Math.max(START, mins)) - START) / (END - START)) * 100}%`

// Only what is ahead is drawn; the solid line behind the disc is all the past needs.
export function DayLine({
  events,
  now,
  compact = false,
}: {
  events: PlanEvent[]
  now: number
  compact?: boolean
}) {
  const ahead = events.filter(
    (ev) =>
      (ev.status === 'pending' || ev.status === 'snoozed' || ev.status === 'fired') &&
      minutesOf(ev.end_wall_time ?? ev.wall_time) >= now,
  )
  const nextId = ahead[0]?.id
  const hours = compact ? [6, 15, 24] : [6, 9, 12, 15, 18, 21, 24]
  return (
    <div className={`dayline${compact ? ' compact' : ''}`} role="img" aria-label="Today, drawn as a line">
      <span className="dl-line" />
      <span className="dl-gone" style={{ width: pct(now) }} />
      <span className="dl-ticks" />
      {hours.map((h) => (
        <span key={h} className="dl-hour" style={{ left: pct(h * 60) }}>
          {String(h).padStart(2, '0')}
        </span>
      ))}
      {ahead.map((ev) => {
        const a = minutesOf(ev.wall_time)
        const b = minutesOf(ev.end_wall_time ?? ev.wall_time)
        const w = Math.max(1, ((b - a) / (END - START)) * 100)
        return (
          <span key={ev.id} className={`dl-span${ev.id === nextId ? ' next' : ''}`} style={{ left: pct(a), width: `${w}%` }}>
            {!compact && (
              <span className={`dl-label${ev.id === nextId ? ' below' : ' above'}`}>
                {ev.wall_time} – {ev.end_wall_time ?? ev.wall_time} {eventLabel(ev.kind)}
              </span>
            )}
          </span>
        )
      })}
      <span className="dl-now" style={{ left: pct(now) }} />
    </div>
  )
}
