import { eventLabel } from './receipts'
import type { PlanEvent } from './types'

const START = 6 * 60
const END = 24 * 60

export function minutesOf(wall: string): number {
  const [h, m] = wall.split(':')
  return Number(h) * 60 + Number(m)
}

const pct = (mins: number) => `${((Math.min(END, Math.max(START, mins)) - START) / (END - START)) * 100}%`

// What is ahead is a bare span; what is behind is a tick on the gone bar for each
// event the day already settled. Names live in the list beside it: a span and its
// row light up together through `hoverId`.
export function DayLine({
  events,
  now,
  nextId,
  compact = false,
  hoverId = null,
  onHover,
}: {
  events: PlanEvent[]
  now: number
  // Which event the caller's face is on; without one the first still ahead is filled.
  nextId?: number
  compact?: boolean
  hoverId?: number | null
  onHover?: (id: number | null) => void
}) {
  const ahead = events.filter(
    (ev) =>
      (ev.status === 'pending' || ev.status === 'snoozed' || ev.status === 'fired') &&
      minutesOf(ev.end_wall_time ?? ev.wall_time) >= now,
  )
  const settled = events.filter(
    (ev) =>
      (ev.status === 'done' || ev.status === 'dropped') &&
      minutesOf(ev.end_wall_time ?? ev.wall_time) <= now,
  )
  const filled = nextId ?? ahead[0]?.id
  const hours = compact ? [6, 15, 24] : [6, 9, 12, 15, 18, 21, 24]

  return (
    <div className={`dayline${compact ? ' compact' : ''}`} role="img" aria-label="Today, drawn as a line">
      <span className="dl-line" />
      <span className="dl-gone" style={{ width: pct(now) }} />
      <span className="dl-ticks" />
      {hours.map((h, i) => (
        <span
          key={h}
          className={`dl-hour${i === 0 ? ' edge-start' : i === hours.length - 1 ? ' edge-end' : ''}`}
          style={{ left: pct(h * 60) }}
        >
          {String(h).padStart(2, '0')}
        </span>
      ))}
      {settled.map((ev) => (
        <span
          key={`past-${ev.id}`}
          className={`dl-past ${ev.status}`}
          style={{ left: pct(minutesOf(ev.end_wall_time ?? ev.wall_time)) }}
        />
      ))}
      {ahead.map((ev) => {
        const a = minutesOf(ev.wall_time)
        const b = minutesOf(ev.end_wall_time ?? ev.wall_time)
        const w = Math.max(1, ((b - a) / (END - START)) * 100)
        // A trigger is a moment, not a stretch: the ring keeps its own size.
        const trigger = ev.kind === 'trigger'
        const name = ev.task ? ev.task.title : eventLabel(ev.kind)
        return (
          <span
            key={ev.id}
            className={`dl-span${ev.id === filled ? ' next' : ''}${ev.task ? ' task' : ''}${trigger ? ' trigger' : ''}${ev.id === hoverId ? ' hover' : ''}`}
            style={trigger ? { left: pct(a) } : { left: pct(a), width: `${w}%` }}
            title={`${ev.wall_time} – ${ev.end_wall_time ?? ev.wall_time} ${name}`}
            onMouseEnter={() => onHover?.(ev.id)}
            onMouseLeave={() => onHover?.(null)}
          />
        )
      })}
      <span className="dl-now" style={{ left: pct(now) }} />
    </div>
  )
}
