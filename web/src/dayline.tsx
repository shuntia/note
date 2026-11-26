import { SunriseIcon } from './briefSheet'
import { t } from './i18n'
import { eventLabel } from './receipts'
import type { CalendarOccurrence, PlanEvent } from './types'

const START = 6 * 60
const MIDNIGHT = 24 * 60

export function minutesOf(wall: string): number {
  const [h, m] = wall.split(':')
  return Number(h) * 60 + Number(m)
}

// What is ahead is a bare span; what is behind is a tick on the gone bar for each
// event the day already settled, and the calendar's own windows are the ground
// under both. Names live in the list beside it: a span and its row light up
// together through `hoverId`.
export function DayLine({
  events,
  calendar = [],
  now,
  nextId,
  compact = false,
  hoverId = null,
  onHover,
  end = MIDNIGHT,
  onBrief,
}: {
  events: PlanEvent[]
  calendar?: CalendarOccurrence[]
  now: number
  // Which event the caller's face is on; without one the first still ahead is filled.
  nextId?: number
  compact?: boolean
  hoverId?: number | null
  onHover?: (id: number | null) => void
  // Past MIDNIGHT where the work day runs on into the small hours.
  end?: number
  // Given only while today's brief exists; the icon at the line's end reopens it.
  onBrief?: () => void
}) {
  const pct = (mins: number) => `${((Math.min(end, Math.max(START, mins)) - START) / (end - START)) * 100}%`
  const width = (from: number, to: number, least: number) =>
    `${Math.max(least, ((to - from) / (end - START)) * 100)}%`
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
  const last = Math.floor(end / 60)
  const base = compact ? [6, 15, 24] : [6, 9, 12, 15, 18, 21, 24]
  const hours = last > 24 ? [...base.filter((h) => h < 24 || !compact), last] : base

  return (
    <div className={`dayline${compact ? ' compact' : ''}`}>
      <div className="dl-draw" role="img" aria-label={t('dayline.label')}>
        <span className="dl-line" />
        <span className="dl-gone" style={{ width: pct(now) }} />
        <span className="dl-ticks" />
        {hours.map((h, i) => (
          <span
            key={h}
            className={`dl-hour${i === 0 ? ' edge-start' : i === hours.length - 1 ? ' edge-end' : ''}`}
            style={{ left: pct(h * 60) }}
          >
            {String(h > 24 ? h % 24 : h).padStart(2, '0')}
          </span>
        ))}
        {calendar.map((o) =>
          o.kind === 'note' ? (
            <span
              key={`c${o.entry_id}-${o.start}`}
              className="dl-note"
              style={{ left: pct(minutesOf(o.start)) }}
              title={`${o.start} ${o.title}`}
            />
          ) : (
            <span
              key={`c${o.entry_id}-${o.start}`}
              className={`dl-band ${o.kind}${minutesOf(o.end) <= now ? ' past' : ''}`}
              style={{ left: pct(minutesOf(o.start)), width: width(minutesOf(o.start), minutesOf(o.end), 0.6) }}
              title={`${o.start} – ${o.end} ${o.title}`}
            />
          ),
        )}
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
          const name = ev.task ? ev.task.title : eventLabel(ev.kind)
          return (
            <span
              key={ev.id}
              className={`dl-span${ev.id === filled ? ' next' : ''}${ev.task ? ' task' : ''}${ev.id === hoverId ? ' hover' : ''}`}
              style={{ left: pct(a), width: width(a, b, 1) }}
              title={`${ev.wall_time} – ${ev.end_wall_time ?? ev.wall_time} ${name}`}
              onMouseEnter={() => onHover?.(ev.id)}
              onMouseLeave={() => onHover?.(null)}
            />
          )
        })}
        <span className="dl-now" style={{ left: pct(now) }} />
      </div>
      {onBrief && (
        <button type="button" className="dl-brief" aria-label={t('brief.open')} onClick={onBrief}>
          <SunriseIcon />
        </button>
      )}
    </div>
  )
}
