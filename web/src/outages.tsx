import { useCallback, useEffect, useState } from 'react'
import { api } from './api'
import { t, type Key } from './i18n'
import type { Outage } from './types'

const POLL_MS = 60_000

const OUTAGE_KEYS: Record<Outage['code'], Key> = {
  model_unavailable: 'failure.modelUnavailable',
  no_key: 'failure.noKey',
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
// once the server says it has cleared. The missing-key line opens the key setting.
export function OutageBanner({ refresh, openKey }: { refresh: number; openKey: () => void }) {
  const [outages, setOutages] = useState<Outage[]>([])
  const look = useCallback(() => {
    api
      .outages()
      .then((r) => setOutages(r.outages))
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

  const shown = outages.filter((o) => o.code in OUTAGE_KEYS)
  if (shown.length === 0) return null
  return (
    <div className="outage" role="status">
      {shown.map((o) =>
        o.code === 'no_key' ? (
          <p key={o.code}>
            <button type="button" className="link" onClick={openKey}>
              {t(OUTAGE_KEYS[o.code])}
            </button>
          </p>
        ) : (
          <p key={o.code}>{t(OUTAGE_KEYS[o.code])}</p>
        ),
      )}
    </div>
  )
}
