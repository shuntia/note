import { admin } from '../../api'
import { useLoad } from '../../hooks'
import { t } from '../../i18n'
import * as format from '../../i18n/format'
import { Group } from '../../ui/rows'
import { Row, stamp, type Expire } from './common'

function uptime(seconds: number): string {
  const h = Math.floor(seconds / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  return h ? t('admin.hoursMinutes', { h, m }) : t('admin.minutes', { m })
}

function size(bytes: number): string {
  return bytes >= 1024 * 1024
    ? `${format.number(bytes / 1024 / 1024, { minimumFractionDigits: 1, maximumFractionDigits: 1 })} MiB`
    : `${format.number(Math.round(bytes / 1024))} KiB`
}

const provider = (p: { kind: string; model: string } | null) =>
  p ? `${p.kind} · ${p.model}` : 'mock'

export function StatusGroup({ expire }: { expire: Expire }) {
  const [status] = useLoad(admin.status, [], { expire })

  if (status === undefined) return null
  if (status === 'error')
    return (
      <Group head={t('admin.head.status')}>
        <p className="set-sub">{t('admin.statusFailed')}</p>
      </Group>
    )

  return (
    <Group head={t('admin.head.status')}>
      <Row label={t('admin.version')} value={status.version} />
      <Row label={t('admin.build')} value={status.build} />
      <Row label={t('admin.started')} value={stamp(status.started_at)} />
      <Row label={t('admin.uptime')} value={uptime(status.uptime_s)} />
      <Row label={t('admin.database')} value={size(status.db_bytes)} />
      <Row label={t('admin.users')} value={format.number(status.users)} />
      <Row label={t('admin.sessions')} value={format.number(status.sessions)} />
      <Row label={t('admin.pushSubscriptions')} value={format.number(status.push_subscriptions)} />
      <Row label={t('admin.llm')} value={provider(status.providers.llm)} />
      <Row label={t('admin.embeddings')} value={provider(status.providers.embeddings)} />
      <Row label={t('admin.webPush')} value={status.webpush ? t('admin.on') : t('admin.off')} />
      <Row
        label={t('admin.adminSecret')}
        value={status.secrets.admin_totp ? t('admin.installed') : t('admin.missing')}
      />
    </Group>
  )
}
