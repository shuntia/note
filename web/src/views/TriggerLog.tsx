import { t } from '../i18n'
import { day } from '../i18n/format'
import type { TriggerLogEntry } from '../types'

type Shape = 'waiting' | 'spoke' | 'quiet' | 'off'

function shapeOf(e: TriggerLogEntry): Shape {
  if (e.status === 'pending' || e.status === 'snoozed') return 'waiting'
  if (e.outcome === 'said' || e.outcome === 'rang') return 'spoke'
  if (e.outcome === 'quiet') return 'quiet'
  return 'off'
}

function ending(e: TriggerLogEntry, shape: Shape) {
  const why = (label: string) => (
    <p className="trig-end">
      {label}
      {e.text && <span className="trig-why"> · {e.text}</span>}
    </p>
  )
  if (shape === 'spoke')
    return (
      <>
        <p className="turn assistant trig-said">{e.text}</p>
        {e.outcome === 'rang' && <p className="trig-end">{t('talk.triggers.rang')}</p>}
      </>
    )
  if (shape === 'quiet') return why(t('talk.triggers.quiet'))
  if (e.outcome === 'cancelled') return why(t('talk.triggers.cancelled'))
  if (shape === 'off' && e.status === 'dropped') return why(t('talk.triggers.dropped'))
  return null
}

export function TriggerLog({ entries }: { entries: TriggerLogEntry[] }) {
  if (entries.length === 0) return <p className="chat-empty">{t('talk.triggers.empty')}</p>
  return (
    <ol className="trig-log">
      {entries.map((e) => {
        const shape = shapeOf(e)
        return (
          <li key={e.event_id} className="trig" data-shape={shape}>
            <time className="trig-when" dateTime={`${e.date}T${e.at}`}>
              {day(new Date(`${e.date}T00:00`))} {e.at}
            </time>
            <p className="trig-prompt">{e.prompt}</p>
            {ending(e, shape)}
          </li>
        )
      })}
    </ol>
  )
}
