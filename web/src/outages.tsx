import { useCallback, useEffect, useState } from 'react'
import { api } from './api'
import { t, type Key } from './i18n'
import type { Outage } from './types'

const POLL_MS = 60_000

const OUTAGE_KEYS: Record<Outage['code'], Key> = {
  model_unavailable: 'failure.modelUnavailable',
  auth_invalid: 'failure.authInvalid',
  out_of_credits: 'failure.outOfCredits',
  rate_limited: 'failure.rateLimited',
  provider_down: 'failure.providerDown',
  context_too_long: 'failure.contextTooLong',
  refused: 'failure.refused',
  internal: 'failure.internal',
  daily_cap: 'outage.dailyCap',
  matrix_down: 'outage.matrix',
  voice_down: 'outage.voice',
}

export function outageLines(outages: Outage[]): string[] {
  return outages.flatMap((o) => (o.code in OUTAGE_KEYS ? [t(OUTAGE_KEYS[o.code])] : []))
}

// Stands over every view while something Note leans on is failing, and goes
// once the server says it has cleared.
export function OutageBanner({ refresh }: { refresh: number }) {
  const [lines, setLines] = useState<string[]>([])
  const look = useCallback(() => {
    api
      .outages()
      .then((r) => setLines(outageLines(r.outages)))
      .catch(() => {})
  }, [])

  useEffect(look, [look, refresh])
  useEffect(() => {
    const id = window.setInterval(look, POLL_MS)
    window.addEventListener('focus', look)
    return () => {
      window.clearInterval(id)
      window.removeEventListener('focus', look)
    }
  }, [look])

  if (lines.length === 0) return null
  return (
    <div className="outage" role="status">
      {lines.map((line) => (
        <p key={line}>{line}</p>
      ))}
    </div>
  )
}
