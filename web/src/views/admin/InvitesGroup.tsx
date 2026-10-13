import { useState, type FormEvent } from 'react'
import { admin } from '../../api'
import { useLoad, useSave } from '../../hooks'
import { t } from '../../i18n'
import * as format from '../../i18n/format'
import type { InviteCreated } from '../../types'
import { FoldRow, Group, ToggleRow } from '../../ui/rows'
import { message, roleName, Status, type Expire, type Notify } from './common'

const INVITE_DAYS = [1, 7, 30] as const

export function InvitesGroup({ notify, expire }: { notify: Notify; expire: Expire }) {
  const [invites, load] = useLoad(admin.invites, [], { expire })
  const [open, setOpen] = useState(false)
  const [fresh, setFresh] = useState<InviteCreated | null>(null)
  const [arming, setArming] = useState<number | null>(null)

  const revoke = async (id: number) => {
    setArming(null)
    try {
      await admin.revokeInvite(id)
      if (fresh?.id === id) setFresh(null)
      load()
    } catch (err) {
      if (!expire(err)) notify(message(err, t('admin.workFailed')))
    }
  }

  const copy = (url: string) =>
    void navigator.clipboard?.writeText(url).then(
      () => notify(t('admin.copied')),
      () => {},
    )

  if (invites === undefined) return null

  return (
    <Group head={t('admin.head.invites')}>
      {invites === 'error' && (
        <p className="set-sub">
          {t('admin.invitesFailed')}{' '}
          <button className="set-link" onClick={load}>
            {t('common.retry')}
          </button>
        </p>
      )}
      {fresh && (
        <div className="set-row set-token-row">
          <span className="set-row-body">
            <code className="set-token-secret">{fresh.url}</code>
            <span className="set-sub">{t('admin.inviteOnce')}</span>
          </span>
          <button className="btn-haze small" onClick={() => copy(fresh.url)}>
            {t('admin.copy')}
          </button>
        </div>
      )}
      {invites !== 'error' &&
        invites.map((i) => (
          <div className="set-row set-token-row" key={i.id}>
            <span className="set-row-body">
              <span className="set-label">{i.username ?? t('admin.inviteAnyName')}</span>
              <span className="set-sub">
                {roleName(i.admin ? 'admin' : 'member')} ·{' '}
                {t('admin.inviteUntil', { date: format.monthDay(new Date(i.expires_at)) })}
              </span>
            </span>
            <button
              type="button"
              className="btn-haze small"
              onClick={() => (arming === i.id ? void revoke(i.id) : setArming(i.id))}
              onBlur={() => setArming((a) => (a === i.id ? null : a))}
            >
              {arming === i.id ? t('admin.revokeSure') : t('admin.revokeInvite')}
            </button>
          </div>
        ))}
      <FoldRow label={t('admin.newInvite')} open={open} onToggle={() => setOpen((o) => !o)}>
        {open && (
          <NewInvite
            expire={expire}
            onCreated={(made) => {
              setFresh(made)
              setOpen(false)
              copy(made.url)
              load()
            }}
          />
        )}
      </FoldRow>
    </Group>
  )
}

function NewInvite({ expire, onCreated }: { expire: Expire; onCreated: (made: InviteCreated) => void }) {
  const [username, setUsername] = useState('')
  const [isAdmin, setIsAdmin] = useState(false)
  const [days, setDays] = useState<number>(7)
  const { save, busy, run } = useSave()

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    await run(
      async () => {
        const name = username.trim()
        onCreated(await admin.createInvite({ admin: isAdmin, days, ...(name ? { username: name } : {}) }))
        return null
      },
      (err) => (expire(err) ? null : message(err, t('admin.saveFailed'))),
    )
  }

  return (
    <form className="set-fold-body" onSubmit={(e) => void submit(e)}>
      <input
        placeholder={t('admin.inviteName')}
        aria-label={t('admin.inviteName')}
        autoComplete="off"
        maxLength={64}
        value={username}
        onChange={(e) => setUsername(e.target.value)}
      />
      <ToggleRow
        className="admin-switch"
        label={t('admin.admin')}
        aria={t('admin.inviteIsAdmin')}
        on={isAdmin}
        onToggle={() => setIsAdmin((a) => !a)}
      />
      <div className="set-row">
        <span className="set-row-body">
          <span className="set-label">{t('admin.inviteOpenFor')}</span>
        </span>
        <select aria-label={t('admin.inviteOpenFor')} value={days} onChange={(e) => setDays(Number(e.target.value))}>
          {INVITE_DAYS.map((d) => (
            <option key={d} value={d}>
              {t('admin.inviteDays', { count: d })}
            </option>
          ))}
        </select>
      </div>
      <div className="set-acts">
        <button className="btn-haze small" disabled={busy}>
          {t('admin.create')}
        </button>
        <Status save={save} />
      </div>
    </form>
  )
}
