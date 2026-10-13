import { admin, ApiError } from '../api'
import { useLoad } from '../hooks'
import { t } from '../i18n'
import * as format from '../i18n/format'
import '../styles/settings.css'
import type { Me } from '../types'
import { Gate } from './admin/Gate'
import { InspectGroup } from './admin/InspectGroup'
import { InvitesGroup } from './admin/InvitesGroup'
import { LogGroup } from './admin/LogGroup'
import { StatusGroup } from './admin/StatusGroup'
import { TracesGroup } from './admin/TracesGroup'
import { UsersGroup } from './admin/UsersGroup'
import type { Expire, Notify } from './admin/common'

export function Admin({ me, notify, onBack }: { me: Me; notify: Notify; onBack: () => void }) {
  const [gate, load, setGate] = useLoad(admin.gate, [])

  const expire: Expire = (err) => {
    if (!(err instanceof ApiError) || err.status !== 401) return false
    setGate((g) => (g && g !== 'error' ? { ...g, elevated: false, expires_at: undefined } : g))
    notify(t('admin.expired'))
    return true
  }

  if (gate === undefined) return null
  if (gate === 'error')
    return (
      <div className="settings admin">
        <p className="set-sub">
          {t('admin.panelFailed')}{' '}
          <button className="set-link" onClick={load}>
            {t('common.retry')}
          </button>
        </p>
      </div>
    )

  if (!gate.elevated) return <Gate gate={gate} onElevated={load} onBack={onBack} />

  const lock = async () => {
    try {
      await admin.drop()
    } catch {
      // the grant is unusable either way; the gate comes back
    }
    setGate({ ...gate, elevated: false, expires_at: undefined })
  }

  return (
    <div className="settings admin">
      <div className="admin-head">
        <button className="set-link" onClick={onBack}>
          ← {t('nav.settings')}
        </button>
        <span className="admin-head-right">
          {gate.expires_at && (
            <span className="set-sub">
              {t('admin.lockedUntil', { time: format.clock(new Date(gate.expires_at)) })}
            </span>
          )}
          <button className="set-link" onClick={() => void lock()}>
            {t('admin.lockNow')}
          </button>
        </span>
      </div>

      {gate.inspect && (
        <div className="admin-dev-banner">
          <b>{t('admin.devBanner')}</b>
          <span>{t('admin.devBannerBody')}</span>
        </div>
      )}

      <StatusGroup expire={expire} />
      <UsersGroup me={me} notify={notify} expire={expire} />
      <InvitesGroup notify={notify} expire={expire} />
      <LogGroup expire={expire} />
      <TracesGroup expire={expire} />
      {gate.inspect && <InspectGroup expire={expire} />}
    </div>
  )
}
