import { useState, type FormEvent } from 'react'
import { admin, ApiError } from '../../api'
import { useLoad, useSave } from '../../hooks'
import { t } from '../../i18n'
import type { AdminUser, Me } from '../../types'
import { FoldRow, Group, ToggleRow } from '../../ui/rows'
import { message, roleName, Status, type Expire, type Notify } from './common'

export function UsersGroup({ me, notify, expire }: { me: Me; notify: Notify; expire: Expire }) {
  const [users, load] = useLoad(admin.users, [], { expire })
  const [open, setOpen] = useState<string | null>(null)

  const fold = (id: string) => () => setOpen((o) => (o === id ? null : id))

  if (users === undefined) return null
  if (users === 'error')
    return (
      <Group head={t('admin.head.users')}>
        <p className="set-sub">
          {t('admin.usersFailed')}{' '}
          <button className="set-link" onClick={load}>
            {t('common.retry')}
          </button>
        </p>
      </Group>
    )

  return (
    <Group head={t('admin.head.users')}>
      {users.map((u) => (
        <UserFold
          key={u.id}
          user={u}
          self={u.username === me.username}
          open={open === `u${u.id}`}
          onToggle={fold(`u${u.id}`)}
          notify={notify}
          expire={expire}
          onChanged={load}
        />
      ))}
      <FoldRow label={t('admin.addUser')} open={open === 'new'} onToggle={fold('new')}>
        {open === 'new' && (
          <AddUser
            expire={expire}
            onCreated={() => {
              setOpen(null)
              load()
            }}
          />
        )}
      </FoldRow>
    </Group>
  )
}

function UserFold({
  user,
  self,
  open,
  onToggle,
  notify,
  expire,
  onChanged,
}: {
  user: AdminUser
  self: boolean
  open: boolean
  onToggle: () => void
  notify: Notify
  expire: Expire
  onChanged: () => void
}) {
  const [password, setPassword] = useState('')
  const { save, run } = useSave()
  const [busy, setBusy] = useState(false)

  const patch = async (p: { role?: 'admin' | 'member'; disabled?: boolean; password?: string }) => {
    setBusy(true)
    try {
      await admin.patchUser(user.id, p)
      onChanged()
      return true
    } catch (err) {
      if (!expire(err)) notify(message(err, t('admin.saveFailed')))
      return false
    } finally {
      setBusy(false)
    }
  }

  const savePassword = async (e: FormEvent) => {
    e.preventDefault()
    if (!password) return
    await run(
      async () => {
        if (!(await patch({ password }))) return null
        setPassword('')
      },
      () => null,
    )
  }

  const revoke = async () => {
    setBusy(true)
    try {
      const { revoked } = await admin.revokeSessions(user.id)
      notify(t('admin.revoked', { count: revoked }))
      onChanged()
    } catch (err) {
      if (!expire(err)) notify(message(err, t('admin.workFailed')))
    } finally {
      setBusy(false)
    }
  }

  return (
    <FoldRow
      label={user.username}
      value={user.disabled ? t('admin.roleDisabled', { role: roleName(user.role) }) : roleName(user.role)}
      sub={t('admin.sessionCount', { count: user.sessions })}
      open={open}
      onToggle={onToggle}
    >
      {open && (
        <div className="set-fold-body">
          <form className="admin-form" onSubmit={savePassword}>
            <input
              type="password"
              placeholder={t('admin.newPassword')}
              autoComplete="new-password"
              aria-label={t('admin.newPasswordFor', { name: user.username })}
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
            <button className="btn-haze small" disabled={!password || busy}>
              {t('common.save')}
            </button>
            <Status save={save} />
          </form>
          <ToggleRow
            className="admin-switch"
            label={t('admin.admin')}
            sub={self ? t('admin.thatsYou') : undefined}
            aria={t('admin.isAdmin', { name: user.username })}
            on={user.role === 'admin'}
            disabled={self || busy}
            onToggle={() => void patch({ role: user.role === 'admin' ? 'member' : 'admin' })}
          />
          <ToggleRow
            className="admin-switch"
            label={t('admin.disabled')}
            sub={self ? t('admin.thatsYou') : undefined}
            aria={t('admin.isDisabled', { name: user.username })}
            on={user.disabled}
            disabled={self || busy}
            onToggle={() => void patch({ disabled: !user.disabled })}
          />
          <button className="set-link" disabled={busy} onClick={() => void revoke()}>
            {t('admin.signOutEverywhere')}
          </button>
        </div>
      )}
    </FoldRow>
  )
}

function AddUser({ expire, onCreated }: { expire: Expire; onCreated: () => void }) {
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [isAdmin, setIsAdmin] = useState(false)
  const { save, busy, run } = useSave()

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    await run(
      async () => {
        await admin.createUser(username.trim(), password, isAdmin)
        setUsername('')
        setPassword('')
        setIsAdmin(false)
        onCreated()
        return null
      },
      (err) => {
        if (expire(err)) return null
        const status = err instanceof ApiError ? err.status : 0
        return status === 409 ? t('admin.usernameTaken') : message(err, t('admin.saveFailed'))
      },
    )
  }

  return (
    <form className="set-fold-body" onSubmit={submit}>
      <input
        placeholder={t('admin.username')}
        autoComplete="off"
        aria-label={t('admin.username')}
        value={username}
        onChange={(e) => setUsername(e.target.value)}
      />
      <input
        type="password"
        placeholder={t('admin.password')}
        autoComplete="new-password"
        aria-label={t('admin.password')}
        value={password}
        onChange={(e) => setPassword(e.target.value)}
      />
      <ToggleRow
        className="admin-switch"
        label={t('admin.admin')}
        aria={t('admin.newUserIsAdmin')}
        on={isAdmin}
        onToggle={() => setIsAdmin((a) => !a)}
      />
      <div className="set-acts">
        <button className="btn-haze small" disabled={busy || !username.trim() || !password}>
          {t('admin.create')}
        </button>
        <Status save={save} />
      </div>
    </form>
  )
}
