import { ApiError } from '../../api'
import type { ViewProps } from '../../app'
import type { Save } from '../../hooks'
import { t } from '../../i18n'
import * as format from '../../i18n/format'
import { Status as RowStatus } from '../../ui/rows'

export type Notify = ViewProps['notify']

// Every panel call funnels through this: `true` means elevation is gone and the
// caller's own error handling is off the hook.
export type Expire = (err: unknown) => boolean

export const LOG_PAGE = 50

export const stamp = (iso: string) => format.dateTime(new Date(iso))

export const roleName = (role: 'admin' | 'member') =>
  role === 'admin' ? t('admin.role.admin') : t('admin.role.member')

export const message = (err: unknown, fallback: string) =>
  err instanceof ApiError && err.message ? err.message : fallback

export function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="set-row">
      <span className="set-row-body">
        <span className="set-label">{label}</span>
      </span>
      <span className="set-value">{value}</span>
    </div>
  )
}

export function Status({ save }: { save: Save }) {
  return <RowStatus save={save} saved={t('admin.saved')} />
}
