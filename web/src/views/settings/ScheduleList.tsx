import { t } from '../../i18n'
import { eventLabel } from '../../receipts'
import type { ScheduleRow } from '../../types'
import { Switch } from '../../ui/rows'

// Only a real slide window is worth a line; everything else the day already shows.
function slideWord(row: ScheduleRow): string | null {
  if (row.entry === 'block' || row.flexibility === 'fixed' || row.slide_window_min <= 0) return null
  const min = row.slide_window_min
  const span = min % 60 === 0 ? t('time.hours', { n: min / 60 }) : t('time.minutes', { n: min })
  return `± ${span}`
}

export function ScheduleList({
  rows,
  busy,
  toggle,
}: {
  rows: ScheduleRow[]
  busy: boolean
  toggle: (row: ScheduleRow) => void
}) {
  if (rows.length === 0) return null
  return (
    <ul className="sched-list">
      {rows.map((row) => {
        const slide = slideWord(row)
        return (
        <li className="sched-row" key={row.index}>
          <span className="sched-time">
            {row.entry === 'block' ? `${row.time}–${row.end_time ?? ''}` : row.time}
          </span>
          <span className="set-row-body">
            <span className="set-label">{eventLabel(row.kind)}</span>
            {slide && <span className="set-sub">{slide}</span>}
          </span>
          {row.entry === 'routine' && (
            <Switch
              label={t('settings.schedule.pings', { name: eventLabel(row.kind) })}
              on={row.alert}
              disabled={busy}
              onToggle={() => toggle(row)}
            />
          )}
        </li>
        )
      })}
    </ul>
  )
}
