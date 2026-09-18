import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { eventLabel } from './receipts'
import type { PlanEvent } from './types'

const START = 6 * 60
const END = 24 * 60
const ROWS = 3
const GAP = 12

export function minutesOf(wall: string): number {
  const [h, m] = wall.split(':')
  return Number(h) * 60 + Number(m)
}

const pct = (mins: number) => `${((Math.min(END, Math.max(START, mins)) - START) / (END - START)) * 100}%`

type Placement = { row: number; short: boolean; hidden: boolean; shift: number }

// What is ahead is drawn in full; what is behind is a tick on the gone bar for
// each event the day already settled.
export function DayLine({
  events,
  now,
  nextId,
  compact = false,
}: {
  events: PlanEvent[]
  now: number
  // Which event the caller's face is on; without one the first still ahead is filled.
  nextId?: number
  compact?: boolean
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

  const wrap = useRef<HTMLDivElement>(null)
  const [place, setPlace] = useState<Record<number, Placement>>({})
  const [remeasure, setRemeasure] = useState(0)
  const sig = `${filled}|${ahead.map((ev) => `${ev.id}:${ev.wall_time}:${ev.end_wall_time}`).join(',')}`

  useEffect(() => {
    const el = wrap.current
    if (!el) return
    const bump = () => setRemeasure((n) => n + 1)
    const ro = new ResizeObserver(bump)
    ro.observe(el)
    void document.fonts?.ready.then(bump)
    return () => ro.disconnect()
  }, [])

  // Labels claim the lowest row they fit in without touching their neighbour; a label
  // with nowhere to go gives up its name and keeps its time.
  useLayoutEffect(() => {
    const el = wrap.current
    if (!el || compact) return
    const width = el.clientWidth
    const nodes = [...el.querySelectorAll<HTMLElement>('.dl-label.above')]
    for (const node of nodes) {
      node.classList.remove('short', 'hide')
      node.style.transform = ''
    }
    const left = el.getBoundingClientRect().left
    const marks = nodes
      .map((node) => {
        const box = node.getBoundingClientRect()
        const name = node.querySelector('.dl-name')?.getBoundingClientRect().width ?? 0
        return { id: Number(node.dataset.ev), centre: box.left + box.width / 2 - left, full: box.width, bare: box.width - name }
      })
      .sort((a, b) => a.centre - b.centre)

    const ends = new Array<number>(ROWS).fill(-Infinity)
    const next: Record<number, Placement> = {}
    for (const mark of marks) {
      const fit = (w: number) => {
        let x = mark.centre - w / 2
        const shift = x < 0 ? -x : x + w > width ? width - (x + w) : 0
        x += shift
        for (let row = 0; row < ROWS; row++) if (x >= ends[row] + GAP) return { row, shift, x, w }
        return null
      }
      const full = fit(mark.full)
      const spot = full ?? fit(mark.bare)
      if (!spot) {
        next[mark.id] = { row: 0, short: true, hidden: true, shift: 0 }
        continue
      }
      ends[spot.row] = spot.x + spot.w
      next[mark.id] = { row: spot.row, short: full === null, hidden: false, shift: spot.shift }
    }
    setPlace(next)
  }, [sig, compact, remeasure])

  return (
    <div ref={wrap} className={`dayline${compact ? ' compact' : ''}`} role="img" aria-label="Today, drawn as a line">
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
        const spot = place[ev.id]
        const above = ['above', `row${(spot?.row ?? 0) + 1}`, spot?.short ? 'short' : '', spot?.hidden ? 'hide' : '']
        return (
          <span
            key={ev.id}
            className={`dl-span${ev.id === filled ? ' next' : ''}${ev.task ? ' task' : ''}`}
            style={{ left: pct(a), width: `${w}%` }}
          >
            {!compact && (
              <span
                className={`dl-label ${ev.id === filled ? 'below' : above.filter(Boolean).join(' ')}`}
                data-ev={ev.id}
                style={spot?.shift ? { transform: `translateX(calc(-50% + ${Math.round(spot.shift)}px))` } : undefined}
              >
                <span className="dl-when">
                  {ev.wall_time} – {ev.end_wall_time ?? ev.wall_time}
                </span>
                <span className="dl-name">{' '}{ev.task ? ev.task.title : eventLabel(ev.kind)}</span>
              </span>
            )}
          </span>
        )
      })}
      <span className="dl-now" style={{ left: pct(now) }} />
    </div>
  )
}
