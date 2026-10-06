import { useEffect, useState, type FormEvent, type ReactNode } from 'react'
import { admin, ApiError } from '../api'
import type { ToastAction } from '../app'
import { t } from '../i18n'
import * as format from '../i18n/format'
import '../styles/settings.css'
import { signChallenge } from '../webauthn'
import type {
  AdminGate,
  AdminStatus,
  AdminUser,
  Conversation,
  InspectUser,
  LogRow,
  Me,
  SqlResult,
  TalkMessage,
  TraceCall,
  TraceDetail,
  TraceRow,
} from '../types'

type Notify = (msg: string, action?: ToastAction) => void

// Every panel call funnels through this: `true` means elevation is gone and the
// caller's own error handling is off the hook.
type Expire = (err: unknown) => boolean

type Save = { kind: 'busy' | 'saved' | 'failed'; message?: string } | null

const LOG_PAGE = 50

const OUTCOMES = ['ok', 'max_turns', 'error'] as const

const stamp = (iso: string) => format.dateTime(new Date(iso))

const unit = (n: number, name: string, digits = 0) =>
  format.number(n, {
    style: 'unit',
    unit: name,
    unitDisplay: 'narrow',
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  })

function duration(ms: number): string {
  if (ms < 1000) return unit(ms, 'millisecond')
  if (ms < 60_000) return unit(ms / 1000, 'second', 1)
  const seconds = Math.round(ms / 1000)
  return t('admin.minutesSeconds', { m: Math.floor(seconds / 60), s: seconds % 60 })
}

function pretty(raw: string): string {
  const text = raw.trim()
  if (!text) return ''
  try {
    return JSON.stringify(JSON.parse(text), null, 2)
  } catch {
    return raw
  }
}

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

const roleName = (role: 'admin' | 'member') =>
  role === 'admin' ? t('admin.role.admin') : t('admin.role.member')

const message = (err: unknown, fallback: string) =>
  err instanceof ApiError && err.message ? err.message : fallback

function Group({ head, children }: { head: string; children: ReactNode }) {
  return (
    <section className="set-group">
      <h2 className="set-group-head">{head}</h2>
      {children}
    </section>
  )
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="set-row">
      <span className="set-row-body">
        <span className="set-label">{label}</span>
      </span>
      <span className="set-value">{value}</span>
    </div>
  )
}

function Status({ save }: { save: Save }) {
  if (!save || save.kind === 'busy') return null
  if (save.kind === 'saved')
    return (
      <span className="pane-status ok" role="status">
        {t('admin.saved')}
      </span>
    )
  return (
    <span className="pane-status bad" role="alert">
      {save.message}
    </span>
  )
}

function Switch({
  label,
  on,
  disabled,
  onToggle,
}: {
  label: string
  on: boolean
  disabled?: boolean
  onToggle?: () => void
}) {
  return (
    <button
      type="button"
      role="switch"
      className="sw"
      aria-checked={on}
      aria-label={label}
      disabled={disabled}
      onClick={onToggle}
    />
  )
}

function FoldRow({
  label,
  value,
  sub,
  open,
  onToggle,
  children,
}: {
  label: string
  value?: string
  sub?: string
  open: boolean
  onToggle: () => void
  children: ReactNode
}) {
  return (
    <div className="set-fold">
      <button className="set-row set-open" aria-expanded={open} onClick={onToggle}>
        <span className="set-row-body">
          <span className="set-label">{label}</span>
          {sub && <span className="set-sub">{sub}</span>}
        </span>
        {value && <span className="set-value">{value}</span>}
        <svg className="set-chev" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M9 6l6 6-6 6" />
        </svg>
      </button>
      {children}
    </div>
  )
}

export function Admin({ me, notify, onBack }: { me: Me; notify: Notify; onBack: () => void }) {
  const [gate, setGate] = useState<AdminGate | 'error' | undefined>(undefined)

  const load = () => {
    admin
      .gate()
      .then(setGate)
      .catch(() => setGate('error'))
  }
  useEffect(load, [])

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
      <LogGroup expire={expire} />
      <TracesGroup expire={expire} />
      {gate.inspect && <InspectGroup expire={expire} />}
    </div>
  )
}

function Gate({
  gate,
  onElevated,
  onBack,
}: {
  gate: AdminGate
  onElevated: () => void
  onBack: () => void
}) {
  const [password, setPassword] = useState('')
  const [code, setCode] = useState('')
  const [useCode, setUseCode] = useState(!gate.methods.passkey)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const { passkey, totp } = gate.methods
  const nothingEnrolled = gate.require_second_factor && !passkey && !totp
  const needsCode = gate.require_second_factor && useCode && totp
  const unenrolled = t('admin.unenrolled')

  const attempt = async (run: () => Promise<void>) => {
    setBusy(true)
    setError(null)
    try {
      await run()
      setPassword('')
      setCode('')
      onElevated()
    } catch (err) {
      const status = err instanceof ApiError ? err.status : 0
      if (status === 401) setError(t('admin.wrong'))
      else if (status === 429) setError(t('admin.throttled'))
      else if (status === 503) setError(unenrolled)
      else if (err instanceof DOMException) setError(t('admin.passkeyUnconfirmed'))
      else setError(t('admin.unlockFailed'))
    } finally {
      setBusy(false)
    }
  }

  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (gate.require_second_factor && passkey && !useCode) {
      void attempt(async () => {
        const challenge = await admin.elevateChallenge()
        await admin.elevateWithPasskey(password, await signChallenge(challenge))
      })
      return
    }
    void attempt(() => admin.elevate(password, needsCode ? code : undefined))
  }

  return (
    <div className="login admin-gate">
      <h1>{t('admin.admin')}</h1>
      {nothingEnrolled ? (
        <p className="admin-gate-note" role="alert">
          {unenrolled}
        </p>
      ) : (
        <form onSubmit={submit}>
          <div className="field">
            <input
              type="password"
              placeholder={t('admin.password')}
              autoComplete="current-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          </div>
          {needsCode && (
            <div className="field">
              <input
                placeholder={t('admin.code')}
                inputMode="numeric"
                autoComplete="one-time-code"
                pattern="\d{6}"
                maxLength={6}
                value={code}
                onChange={(e) => setCode(e.target.value)}
              />
            </div>
          )}
          {!gate.require_second_factor && gate.inspect && (
            <p className="admin-gate-dev">{t('admin.devPasswordOnly')}</p>
          )}
          {error && <p role="alert">{error}</p>}
          <button
            className="primary"
            disabled={busy || !password || (needsCode && code.length !== 6)}
          >
            {gate.require_second_factor && passkey && !useCode ? t('admin.usePasskey') : t('admin.unlock')}
          </button>
          {gate.require_second_factor && passkey && totp && (
            <button
              type="button"
              className="set-link admin-gate-alt"
              onClick={() => {
                setError(null)
                setUseCode((c) => !c)
              }}
            >
              {useCode ? t('admin.usePasskeyInstead') : t('admin.useCodeInstead')}
            </button>
          )}
        </form>
      )}
      <button className="set-link admin-gate-back" onClick={onBack}>
        ← {t('nav.settings')}
      </button>
    </div>
  )
}

function StatusGroup({ expire }: { expire: Expire }) {
  const [status, setStatus] = useState<AdminStatus | 'error' | undefined>(undefined)

  useEffect(() => {
    admin
      .status()
      .then(setStatus)
      .catch((err) => {
        if (!expire(err)) setStatus('error')
      })
  }, [])

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

function UsersGroup({ me, notify, expire }: { me: Me; notify: Notify; expire: Expire }) {
  const [users, setUsers] = useState<AdminUser[] | 'error' | undefined>(undefined)
  const [open, setOpen] = useState<string | null>(null)

  const load = () => {
    admin
      .users()
      .then(setUsers)
      .catch((err) => {
        if (!expire(err)) setUsers('error')
      })
  }
  useEffect(load, [])

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
  const [save, setSave] = useState<Save>(null)
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
    setSave({ kind: 'busy' })
    const ok = await patch({ password })
    if (ok) {
      setPassword('')
      setSave({ kind: 'saved' })
    } else {
      setSave(null)
    }
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
          <div className="set-row admin-switch">
            <span className="set-row-body">
              <span className="set-label">{t('admin.admin')}</span>
              {self && <span className="set-sub">{t('admin.thatsYou')}</span>}
            </span>
            <Switch
              label={t('admin.isAdmin', { name: user.username })}
              on={user.role === 'admin'}
              disabled={self || busy}
              onToggle={() => void patch({ role: user.role === 'admin' ? 'member' : 'admin' })}
            />
          </div>
          <div className="set-row admin-switch">
            <span className="set-row-body">
              <span className="set-label">{t('admin.disabled')}</span>
              {self && <span className="set-sub">{t('admin.thatsYou')}</span>}
            </span>
            <Switch
              label={t('admin.isDisabled', { name: user.username })}
              on={user.disabled}
              disabled={self || busy}
              onToggle={() => void patch({ disabled: !user.disabled })}
            />
          </div>
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
  const [save, setSave] = useState<Save>(null)

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    setSave({ kind: 'busy' })
    try {
      await admin.createUser(username.trim(), password, isAdmin)
      setUsername('')
      setPassword('')
      setIsAdmin(false)
      setSave(null)
      onCreated()
    } catch (err) {
      if (expire(err)) return
      const status = err instanceof ApiError ? err.status : 0
      setSave({
        kind: 'failed',
        message:
          status === 409
            ? t('admin.usernameTaken')
            : message(err, t('admin.saveFailed')),
      })
    }
  }

  const busy = save?.kind === 'busy'

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
      <div className="set-row admin-switch">
        <span className="set-row-body">
          <span className="set-label">{t('admin.admin')}</span>
        </span>
        <Switch label={t('admin.newUserIsAdmin')} on={isAdmin} onToggle={() => setIsAdmin((a) => !a)} />
      </div>
      <div className="set-acts">
        <button className="btn-haze small" disabled={busy || !username.trim() || !password}>
          {t('admin.create')}
        </button>
        <Status save={save} />
      </div>
    </form>
  )
}

function LogGroup({ expire }: { expire: Expire }) {
  const [rows, setRows] = useState<LogRow[]>([])
  const [kinds, setKinds] = useState<string[]>([])
  const [kind, setKind] = useState('')
  const [state, setState] = useState<'loading' | 'ready' | 'error'>('loading')

  const fetchPage = (which: string, before?: number) => {
    setState('loading')
    admin
      .log({ limit: LOG_PAGE, kind: which || undefined, before_id: before })
      .then((page) => {
        setRows((r) => (before === undefined ? page.rows : [...r, ...page.rows]))
        setKinds(page.kinds)
        setState('ready')
      })
      .catch((err) => {
        if (!expire(err)) setState('error')
      })
  }

  useEffect(() => {
    fetchPage(kind)
  }, [kind])

  const last = rows[rows.length - 1]

  return (
    <Group head={t('admin.head.log')}>
      <div className="set-fold-body">
        <select aria-label={t('admin.kind')} value={kind} onChange={(e) => setKind(e.target.value)}>
          <option value="">{t('admin.all')}</option>
          {kinds.map((k) => (
            <option key={k} value={k}>
              {k}
            </option>
          ))}
        </select>
        {state === 'error' && (
          <p className="set-sub">
            {t('admin.logFailed')}{' '}
            <button className="set-link" onClick={() => fetchPage(kind)}>
              {t('common.retry')}
            </button>
          </p>
        )}
        {rows.length > 0 && (
          <div className="log-scroll">
            <table className="log-table">
              <tbody>
                {rows.map((row) => (
                  <tr key={row.id}>
                    <td className="mono" title={stamp(row.ts)}>
                      {row.ts.slice(11, 19)}
                    </td>
                    <td>{row.kind}</td>
                    <td className="set-sub">{row.detail}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {state === 'ready' && rows.length === 0 && <p className="set-sub">{t('admin.nothingLogged')}</p>}
        <div className="set-acts">
          <button
            className="set-link"
            disabled={state === 'loading' || !last}
            onClick={() => last && fetchPage(kind, last.id)}
          >
            {t('admin.older')}
          </button>
          <button
            className="set-link"
            disabled={state === 'loading'}
            onClick={() => fetchPage(kind)}
          >
            {t('admin.refresh')}
          </button>
        </div>
      </div>
    </Group>
  )
}

const outcomeClass = (outcome: string) =>
  outcome === 'error' ? 'trace-bad' : outcome === 'max_turns' ? 'trace-warn' : 'set-sub'

function TracesGroup({ expire }: { expire: Expire }) {
  const [rows, setRows] = useState<TraceRow[]>([])
  const [kinds, setKinds] = useState<string[]>([])
  const [kind, setKind] = useState('')
  const [outcome, setOutcome] = useState('')
  const [state, setState] = useState<'loading' | 'ready' | 'error'>('loading')
  const [open, setOpen] = useState<number | null>(null)

  const fetchPage = (before?: number) => {
    setState('loading')
    if (before === undefined) setOpen(null)
    admin
      .traces({
        limit: LOG_PAGE,
        kind: kind || undefined,
        outcome: outcome || undefined,
        before_id: before,
      })
      .then((page) => {
        setRows((r) => (before === undefined ? page.rows : [...r, ...page.rows]))
        setKinds(page.kinds)
        setState('ready')
      })
      .catch((err) => {
        if (expire(err)) return
        // A server without the route has simply never recorded one.
        if (err instanceof ApiError && err.status === 404) {
          setRows([])
          setState('ready')
          return
        }
        setState('error')
      })
  }

  useEffect(() => {
    fetchPage()
  }, [kind, outcome])

  const last = rows[rows.length - 1]

  return (
    <Group head={t('admin.head.sessions')}>
      <div className="set-fold-body">
        <div className="trace-filters">
          <select aria-label={t('admin.kind')} value={kind} onChange={(e) => setKind(e.target.value)}>
            <option value="">{t('admin.allKinds')}</option>
            {kinds.map((k) => (
              <option key={k} value={k}>
                {k}
              </option>
            ))}
          </select>
          <select
            aria-label={t('admin.outcome')}
            value={outcome}
            onChange={(e) => setOutcome(e.target.value)}
          >
            <option value="">{t('admin.allOutcomes')}</option>
            {OUTCOMES.map((o) => (
              <option key={o} value={o}>
                {o}
              </option>
            ))}
          </select>
        </div>
        {state === 'error' && (
          <p className="set-sub">
            {t('admin.sessionsFailed')}{' '}
            <button className="set-link" onClick={() => fetchPage()}>
              {t('common.retry')}
            </button>
          </p>
        )}
        {rows.length > 0 && (
          <div className="log-scroll">
            <table className="log-table admin-table trace-table">
              <thead>
                <tr>
                  <th>{t('admin.col.time')}</th>
                  <th>{t('admin.col.user')}</th>
                  <th>{t('admin.col.kind')}</th>
                  <th>{t('admin.col.outcome')}</th>
                  <th>{t('admin.col.turns')}</th>
                  <th>{t('admin.col.tools')}</th>
                  <th>{t('admin.col.took')}</th>
                  <th>{t('admin.col.error')}</th>
                </tr>
              </thead>
              <tbody>
                {/* the time button is the row's keyboard handle; its click bubbles to the row */}
                {rows.map((row) => (
                  <tr
                    key={row.id}
                    className={open === row.id ? 'trace-row on' : 'trace-row'}
                    onClick={() => setOpen((o) => (o === row.id ? null : row.id))}
                  >
                    <td className="mono">
                      <button
                        className="trace-open"
                        aria-expanded={open === row.id}
                        title={stamp(row.ts)}
                      >
                        {row.ts.slice(11, 19)}
                      </button>
                    </td>
                    <td>{row.username}</td>
                    <td>{row.kind}</td>
                    <td className={outcomeClass(row.outcome)}>{row.outcome}</td>
                    <td className="mono">{row.turns}</td>
                    <td className="mono">{row.tool_calls}</td>
                    <td className="mono">{duration(row.duration_ms)}</td>
                    <td className="set-sub trace-err" title={row.error ?? undefined}>
                      {row.error}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {state === 'ready' && rows.length === 0 && (
          <p className="set-sub">{t('admin.noSessions')}</p>
        )}
        {open !== null && <TraceFold id={open} expire={expire} />}
        <div className="set-acts">
          <button
            className="set-link"
            disabled={state === 'loading' || !last}
            onClick={() => last && fetchPage(last.id)}
          >
            {t('admin.older')}
          </button>
          <button className="set-link" disabled={state === 'loading'} onClick={() => fetchPage()}>
            {t('admin.refresh')}
          </button>
        </div>
      </div>
    </Group>
  )
}

function TraceFold({ id, expire }: { id: number; expire: Expire }) {
  const [trace, setTrace] = useState<TraceDetail | 'error' | undefined>(undefined)

  useEffect(() => {
    setTrace(undefined)
    admin
      .trace(id)
      .then(setTrace)
      .catch((err) => {
        if (!expire(err)) setTrace('error')
      })
  }, [id])

  if (trace === undefined) return <p className="set-sub">{t('admin.loading')}</p>
  if (trace === 'error') return <p className="set-sub">{t('admin.sessionFailed')}</p>

  const opening = trace.opening?.trim()
  const reply = trace.reply?.trim()

  return (
    <div className="trace-detail">
      <div className="trace-detail-head">
        <span className="set-label">
          {trace.kind} · {trace.username}
        </span>
        <span className="set-sub">{stamp(trace.ts)}</span>
        <span className={outcomeClass(trace.outcome)}>{trace.outcome}</span>
      </div>
      {trace.error && (
        <p className="pane-status bad" role="alert">
          {trace.error}
        </p>
      )}
      {opening && (
        <div className="trace-text">
          <span className="receipt-tool">{t('admin.opening')}</span>
          <pre className="receipt-block">{opening}</pre>
        </div>
      )}
      {trace.rounds.length === 0 && <p className="set-sub">{t('admin.noRounds')}</p>}
      {trace.rounds.map((round, i) => (
        <div className="trace-round" key={i}>
          <div className="trace-round-head">
            <span className="trace-round-n">{t('admin.round', { n: i + 1 })}</span>
            <span className="set-sub">{duration(round.ms)}</span>
          </div>
          {round.error && <p className="pane-status bad">{round.error}</p>}
          {round.calls?.map((call, j) => <Call key={j} call={call} />)}
        </div>
      ))}
      {reply && (
        <div className="trace-text">
          <span className="receipt-tool">{t('admin.reply')}</span>
          <pre className="receipt-block">{reply}</pre>
        </div>
      )}
      {!trace.full && (
        <p className="set-sub">{t('admin.releaseBuild')}</p>
      )}
    </div>
  )
}

function Call({ call }: { call: TraceCall }) {
  const [open, setOpen] = useState(false)
  const args = pretty(call.args ?? '')
  const result = pretty(call.result ?? '')
  const body = Boolean(args || result)
  const state = [call.is_error ? 'error' : '', open ? 'open' : ''].filter(Boolean).join(' ')

  return (
    <div className={`receipt ${state}`.trim()}>
      <button
        className="receipt-chip"
        aria-expanded={body ? open : undefined}
        disabled={!body}
        onClick={() => setOpen((v) => !v)}
      >
        <svg className="receipt-mark" viewBox="0 0 24 24" aria-hidden="true">
          {call.is_error ? <path d="M6 6l12 12M18 6L6 18" /> : <path d="M5 12.5l4.5 4.5L19 7.5" />}
        </svg>
        <span className="receipt-text">{call.name}</span>
        {call.error_kind && <span className="trace-kind">{call.error_kind}</span>}
        <span className="trace-ms mono">{duration(call.ms)}</span>
        {body && (
          <svg className="receipt-chev" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M9 6l6 6-6 6" />
          </svg>
        )}
      </button>
      {open && body && (
        <div className="receipt-body">
          {args && <pre className="receipt-block">{args}</pre>}
          {result && <pre className="receipt-block">{result}</pre>}
        </div>
      )}
    </div>
  )
}

function InspectGroup({ expire }: { expire: Expire }) {
  const [users, setUsers] = useState<AdminUser[]>([])
  const [id, setId] = useState<number | null>(null)
  const [data, setData] = useState<InspectUser | 'error' | undefined>(undefined)
  const [open, setOpen] = useState<string | null>(null)

  useEffect(() => {
    admin
      .users()
      .then(setUsers)
      .catch((err) => void expire(err))
  }, [])

  useEffect(() => {
    if (id === null) return
    setData(undefined)
    setOpen(null)
    admin
      .inspectUser(id)
      .then(setData)
      .catch((err) => {
        if (!expire(err)) setData('error')
      })
  }, [id])

  const fold = (key: string) => () => setOpen((o) => (o === key ? null : key))
  const loaded = data !== undefined && data !== 'error' ? data : null

  return (
    <Group head={t('admin.head.inspect')}>
      <div className="set-fold-body">
        <select
          aria-label={t('admin.user')}
          value={id === null ? '' : String(id)}
          onChange={(e) => setId(e.target.value ? Number(e.target.value) : null)}
        >
          <option value="">{t('admin.pickUser')}</option>
          {users.map((u) => (
            <option key={u.id} value={u.id}>
              {u.username}
            </option>
          ))}
        </select>
        {data === 'error' && <p className="set-sub">{t('admin.userFailed')}</p>}
      </div>

      {loaded && id !== null && (
        <>
          <FoldRow label={t('admin.config')} open={open === 'config'} onToggle={fold('config')}>
            {open === 'config' && (
              <ConfigFold
                id={id}
                path={loaded.config_path}
                toml={loaded.config_toml}
                expire={expire}
              />
            )}
          </FoldRow>

          <FoldRow
            label={t('admin.tasks')}
            value={format.number(loaded.tasks.length)}
            open={open === 'tasks'}
            onToggle={fold('tasks')}
          >
            {open === 'tasks' && (
              <div className="set-fold-body">
                <div className="log-scroll">
                  <table className="log-table admin-table">
                    <thead>
                      <tr>
                        <th>{t('admin.col.id')}</th>
                        <th>{t('admin.col.title')}</th>
                        <th>{t('admin.col.state')}</th>
                        <th>{t('admin.col.now')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {loaded.tasks.flatMap((task) => [task, ...task.children]).map((task) => (
                        <tr key={task.id}>
                          <td className="mono">{task.id}</td>
                          <td>{task.title}</td>
                          <td className="set-sub">{task.state}</td>
                          <td className="set-sub">{task.is_now ? t('admin.yes') : ''}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>
            )}
          </FoldRow>

          <FoldRow
            label={t('admin.today')}
            value={format.number(loaded.events_today.length)}
            open={open === 'today'}
            onToggle={fold('today')}
          >
            {open === 'today' && (
              <div className="set-fold-body">
                <div className="log-scroll">
                  <table className="log-table admin-table">
                    <thead>
                      <tr>
                        <th>{t('admin.col.time')}</th>
                        <th>{t('admin.col.kind')}</th>
                        <th>{t('admin.col.status')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {loaded.events_today.map((ev) => (
                        <tr key={ev.id}>
                          <td className="mono">{ev.wall_time}</td>
                          <td>{ev.kind}</td>
                          <td className="set-sub">{ev.status}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>
            )}
          </FoldRow>

          <FoldRow
            label={t('admin.conversations')}
            value={format.number(loaded.conversations.length)}
            open={open === 'conversations'}
            onToggle={fold('conversations')}
          >
            {open === 'conversations' && (
              <ConversationsFold id={id} items={loaded.conversations} expire={expire} />
            )}
          </FoldRow>

          <FoldRow
            label={t('admin.memory')}
            value={format.number(loaded.memory.length)}
            open={open === 'memory'}
            onToggle={fold('memory')}
          >
            {open === 'memory' && (
              <MemoryFold id={id} items={loaded.memory} expire={expire} />
            )}
          </FoldRow>
        </>
      )}

      <FoldRow label={t('admin.sql')} open={open === 'sql'} onToggle={fold('sql')}>
        {open === 'sql' && <SqlFold expire={expire} />}
      </FoldRow>
    </Group>
  )
}

function ConfigFold({
  id,
  path,
  toml,
  expire,
}: {
  id: number
  path: string
  toml: string
  expire: Expire
}) {
  const [text, setText] = useState(toml)
  const [save, setSave] = useState<Save>(null)

  const submit = async () => {
    setSave({ kind: 'busy' })
    try {
      await admin.putConfig(id, text)
      setSave({ kind: 'saved' })
    } catch (err) {
      if (expire(err)) return
      setSave({ kind: 'failed', message: message(err, t('admin.saveFailed')) })
    }
  }

  return (
    <div className="set-fold-body">
      <span className="set-sub">{path}</span>
      <textarea
        className="mono set-prompt"
        aria-label={t('admin.userConfig')}
        rows={14}
        value={text}
        onChange={(e) => setText(e.target.value)}
      />
      <div className="set-acts">
        <button
          className="btn-haze small"
          disabled={save?.kind === 'busy'}
          onClick={() => void submit()}
        >
          {t('common.save')}
        </button>
        <Status save={save} />
      </div>
    </div>
  )
}

function ConversationsFold({
  id,
  items,
  expire,
}: {
  id: number
  items: Conversation[]
  expire: Expire
}) {
  const [cid, setCid] = useState<number | null>(null)
  const [messages, setMessages] = useState<TalkMessage[] | 'error' | undefined>(undefined)

  const openOne = (next: number) => {
    setCid(next)
    setMessages(undefined)
    admin
      .conversationMessages(id, next)
      .then(setMessages)
      .catch((err) => {
        if (!expire(err)) setMessages('error')
      })
  }

  return (
    <div className="set-fold-body">
      <ul className="admin-list">
        {items.map((c) => (
          <li key={c.id}>
            <button
              className="set-link"
              aria-current={cid === c.id}
              onClick={() => openOne(c.id)}
            >
              {c.title || t('admin.conversation', { id: c.id })}
            </button>
          </li>
        ))}
      </ul>
      {messages === 'error' && <p className="set-sub">{t('admin.messagesFailed')}</p>}
      {Array.isArray(messages) && (
        <div className="log-scroll admin-msgs">
          {messages.map((m) => (
            <p key={m.id}>
              <b>{m.role}</b> {m.content}
            </p>
          ))}
        </div>
      )}
    </div>
  )
}

function MemoryFold({
  id,
  items,
  expire,
}: {
  id: number
  items: { id: string; category: string; summary: string; archived: boolean }[]
  expire: Expire
}) {
  const [mid, setMid] = useState<string | null>(null)
  const [text, setText] = useState('')
  const [save, setSave] = useState<Save>(null)

  const openOne = (next: string) => {
    setMid(next)
    setText('')
    setSave({ kind: 'busy' })
    admin
      .memoryGet(id, next)
      .then(({ content }) => {
        setText(content)
        setSave(null)
      })
      .catch((err) => {
        if (expire(err)) return
        setSave({ kind: 'failed', message: t('admin.factFailed') })
      })
  }

  const submit = async () => {
    if (!mid) return
    setSave({ kind: 'busy' })
    try {
      await admin.memoryPut(id, mid, text)
      setSave({ kind: 'saved' })
    } catch (err) {
      if (expire(err)) return
      setSave({ kind: 'failed', message: message(err, t('admin.saveFailed')) })
    }
  }

  return (
    <div className="set-fold-body">
      <ul className="admin-list">
        {items.map((m) => (
          <li key={m.id}>
            <button className="set-link" aria-current={mid === m.id} onClick={() => openOne(m.id)}>
              {m.id} · {m.category} · {m.summary}
              {m.archived && <> · {t('admin.archived')}</>}
            </button>
          </li>
        ))}
      </ul>
      {mid && (
        <>
          <textarea
            className="mono set-prompt"
            aria-label={t('admin.memoryFor', { id: mid })}
            rows={10}
            value={text}
            onChange={(e) => setText(e.target.value)}
          />
          <div className="set-acts">
            <button
              className="btn-haze small"
              disabled={save?.kind === 'busy'}
              onClick={() => void submit()}
            >
              {t('common.save')}
            </button>
            <Status save={save} />
          </div>
        </>
      )}
    </div>
  )
}

const cellText = (cell: unknown) => (typeof cell === 'string' ? cell : JSON.stringify(cell))

function SqlFold({ expire }: { expire: Expire }) {
  const [sql, setSql] = useState('')
  const [result, setResult] = useState<SqlResult | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const run = async () => {
    setBusy(true)
    setError(null)
    try {
      setResult(await admin.sql(sql))
    } catch (err) {
      if (expire(err)) return
      setResult(null)
      setError(message(err, t('admin.runFailed')))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="set-fold-body">
      <textarea
        className="mono set-prompt"
        aria-label={t('admin.sql')}
        rows={5}
        spellCheck={false}
        value={sql}
        onChange={(e) => setSql(e.target.value)}
      />
      <div className="set-acts">
        <button className="btn-haze small" disabled={busy || !sql.trim()} onClick={() => void run()}>
          {t('admin.run')}
        </button>
      </div>
      {error && (
        <p className="pane-status bad" role="alert">
          {error}
        </p>
      )}
      {result && 'changes' in result && (
        <p className="set-sub">{t('admin.rowsChanged', { count: result.changes })}</p>
      )}
      {result && 'columns' in result && (
        <>
          <div className="log-scroll">
            <table className="log-table admin-table">
              <thead>
                <tr>
                  {result.columns.map((c) => (
                    <th key={c}>{c}</th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {result.rows.map((row, i) => (
                  <tr key={i}>
                    {row.map((cell, j) =>
                      cell === null ? (
                        <td key={j} className="set-sub">
                          ∅
                        </td>
                      ) : (
                        <td key={j}>{cellText(cell)}</td>
                      ),
                    )}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {result.truncated && <p className="set-sub">{t('admin.truncated', { n: format.number(500) })}</p>}
        </>
      )}
    </div>
  )
}
