import { useContext, useEffect, useState } from 'react'
import { t } from '../../i18n'
import { Status } from '../../ui/rows'
import { SettingsContext } from './ToggleSetting'

const MAX_TIMES = 6

const tidy = (times: string[]) => [...new Set(times.filter(Boolean))].sort()

export function ChoresTimes() {
  const ctx = useContext(SettingsContext)
  const saved = ctx?.settings.chores_times
  const [times, setTimes] = useState<string[]>(saved ?? [])
  useEffect(() => setTimes(saved ?? []), [saved])
  if (!ctx) return null

  const commit = (next: string[]) => {
    const clean = tidy(next)
    if (clean.join() === ctx.settings.chores_times.join()) setTimes(clean)
    else void ctx.saveSettings('chores', { chores_times: clean })
  }

  return (
    <>
      <span className="set-sub">{t('settings.chores.sub')}</span>
      <div className="set-times">
        {times.map((time, i) => (
          <span className="set-time" key={i}>
            <input
              type="time"
              aria-label={t('settings.chores')}
              value={time}
              disabled={ctx.busy}
              onChange={(e) => setTimes(times.map((x, j) => (j === i ? e.target.value : x)))}
              onBlur={() => commit(times)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') e.currentTarget.blur()
              }}
            />
            <button
              type="button"
              className="set-time-remove"
              aria-label={t('settings.chores.remove')}
              disabled={ctx.busy}
              onClick={() => commit(times.filter((_, j) => j !== i))}
            >
              ×
            </button>
          </span>
        ))}
        {times.length < MAX_TIMES && (
          <button
            type="button"
            className="btn-haze small"
            aria-label={t('settings.chores.add')}
            disabled={ctx.busy}
            onClick={() => setTimes([...times, ''])}
          >
            +
          </button>
        )}
      </div>
      <Status save={ctx.save} row="chores" />
    </>
  )
}
