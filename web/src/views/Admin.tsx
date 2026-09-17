import { useEffect, useState, type FormEvent, type ReactNode } from 'react'
import { admin, ApiError } from '../api'
import type { ToastAction } from '../app'
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
} from '../types'

type Notify = (msg: string, action?: ToastAction) => void

// Every panel call funnels through this: `true` means elevation is gone and the
// caller's own error handling is off the hook.
type Expire = (err: unknown) => boolean

type Save = { kind: 'busy' | 'saved' | 'failed'; message?: string } | null

const LOG_PAGE = 50

const clock = (iso: string) =>
  new Date(iso).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })

const stamp = (iso: string) => new Date(iso).toLocaleString()

function uptime(seconds: number): string {
  const h = Math.floor(seconds / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  return h ? `${h} h ${m} m` : `${m} m`
}

function size(bytes: number): string {
  return bytes >= 1024 * 1024
    ? `${(bytes / 1024 / 1024).toFixed(1)} MiB`
    : `${Math.round(bytes / 1024)} KiB`
}

const provider = (p: { kind: string; model: string } | null) =>
  p ? `${p.kind} · ${p.model}` : 'mock'

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
        ✓ Saved
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
    notify('Admin session expired.')
    return true
  }

  if (gate === undefined) return null
  if (gate === 'error')
    return (
      <div className="settings admin">
        <p className="set-sub">
          The panel didn't load.{' '}
          <button className="set-link" onClick={load}>
            Retry
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
          ← Settings
        </button>
        <span className="admin-head-right">
          {gate.expires_at && (
            <span className="set-sub">Locked until {clock(gate.expires_at)}</span>
          )}
          <button className="set-link" onClick={() => void lock()}>
            Lock now
          </button>
        </span>
      </div>

      {gate.inspect && (
        <div className="admin-dev-banner">
          <b>DEV-INSPECT BUILD</b>
          <span>User data is open to inspection and edits. Do not deploy this build.</span>
        </div>
      )}

      <StatusGroup expire={expire} />
      <UsersGroup me={me} notify={notify} expire={expire} />
      <LogGroup expire={expire} />
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
  const unenrolled = 'Add a passkey or an authenticator app in Settings first.'

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
      if (status === 401) setError('Wrong password or code.')
      else if (status === 429) setError('Too many attempts. Wait a few minutes.')
      else if (status === 503) setError(unenrolled)
      else if (err instanceof DOMException) setError('That passkey was not confirmed.')
      else setError("Couldn't unlock. Try again.")
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
      <h1>Admin</h1>
      {nothingEnrolled ? (
        <p className="admin-gate-note" role="alert">
          {unenrolled}
        </p>
      ) : (
        <form onSubmit={submit}>
          <div className="field">
            <input
              type="password"
              placeholder="Password"
              autoComplete="current-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          </div>
          {needsCode && (
            <div className="field">
              <input
                placeholder="6-digit code"
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
            <p className="admin-gate-dev">Dev build: password only</p>
          )}
          {error && <p role="alert">{error}</p>}
          <button
            className="primary"
            disabled={busy || !password || (needsCode && code.length !== 6)}
          >
            {gate.require_second_factor && passkey && !useCode ? 'Use passkey' : 'Unlock'}
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
              {useCode ? 'Use a passkey instead' : 'Use a code instead'}
            </button>
          )}
        </form>
      )}
      <button className="set-link admin-gate-back" onClick={onBack}>
        ← Settings
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
      <Group head="STATUS">
        <p className="set-sub">Status didn't load.</p>
      </Group>
    )

  return (
    <Group head="STATUS">
      <Row label="Version" value={status.version} />
      <Row label="Build" value={status.build} />
      <Row label="Started" value={stamp(status.started_at)} />
      <Row label="Uptime" value={uptime(status.uptime_s)} />
      <Row label="Database" value={size(status.db_bytes)} />
      <Row label="Users" value={String(status.users)} />
      <Row label="Sessions" value={String(status.sessions)} />
      <Row label="Push subscriptions" value={String(status.push_subscriptions)} />
      <Row label="LLM" value={provider(status.providers.llm)} />
      <Row label="Embeddings" value={provider(status.providers.embeddings)} />
      <Row label="Web push" value={status.webpush ? 'on' : 'off'} />
      <Row label="Admin secret" value={status.secrets.admin_totp ? 'installed' : 'missing'} />
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
      <Group head="USERS">
        <p className="set-sub">
          Users didn't load.{' '}
          <button className="set-link" onClick={load}>
            Retry
          </button>
        </p>
      </Group>
    )

  return (
    <Group head="USERS">
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
      <FoldRow label="Add user" open={open === 'new'} onToggle={fold('new')}>
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
      if (!expire(err)) notify(message(err, "That didn't save. Try again."))
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
      notify(`Signed out ${revoked} sessions`)
      onChanged()
    } catch (err) {
      if (!expire(err)) notify(message(err, "That didn't work. Try again."))
    } finally {
      setBusy(false)
    }
  }

  return (
    <FoldRow
      label={user.username}
      value={user.role + (user.disabled ? ' · disabled' : '')}
      sub={`${user.sessions} session${user.sessions === 1 ? '' : 's'}`}
      open={open}
      onToggle={onToggle}
    >
      {open && (
        <div className="set-fold-body">
          <form className="admin-form" onSubmit={savePassword}>
            <input
              type="password"
              placeholder="New password"
              autoComplete="new-password"
              aria-label={`New password for ${user.username}`}
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
            <button className="btn-haze small" disabled={!password || busy}>
              Save
            </button>
            <Status save={save} />
          </form>
          <div className="set-row admin-switch">
            <span className="set-row-body">
              <span className="set-label">Admin</span>
              {self && <span className="set-sub">That's you.</span>}
            </span>
            <Switch
              label={`${user.username} is an admin`}
              on={user.role === 'admin'}
              disabled={self || busy}
              onToggle={() => void patch({ role: user.role === 'admin' ? 'member' : 'admin' })}
            />
          </div>
          <div className="set-row admin-switch">
            <span className="set-row-body">
              <span className="set-label">Disabled</span>
              {self && <span className="set-sub">That's you.</span>}
            </span>
            <Switch
              label={`${user.username} is disabled`}
              on={user.disabled}
              disabled={self || busy}
              onToggle={() => void patch({ disabled: !user.disabled })}
            />
          </div>
          <button className="set-link" disabled={busy} onClick={() => void revoke()}>
            Sign out everywhere
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
            ? 'That username is taken.'
            : message(err, "That didn't save. Try again."),
      })
    }
  }

  const busy = save?.kind === 'busy'

  return (
    <form className="set-fold-body" onSubmit={submit}>
      <input
        placeholder="Username"
        autoComplete="off"
        aria-label="Username"
        value={username}
        onChange={(e) => setUsername(e.target.value)}
      />
      <input
        type="password"
        placeholder="Password"
        autoComplete="new-password"
        aria-label="Password"
        value={password}
        onChange={(e) => setPassword(e.target.value)}
      />
      <div className="set-row admin-switch">
        <span className="set-row-body">
          <span className="set-label">Admin</span>
        </span>
        <Switch label="New user is an admin" on={isAdmin} onToggle={() => setIsAdmin((a) => !a)} />
      </div>
      <div className="set-acts">
        <button className="btn-haze small" disabled={busy || !username.trim() || !password}>
          Create
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
    <Group head="SERVER LOG">
      <div className="set-fold-body">
        <select aria-label="Kind" value={kind} onChange={(e) => setKind(e.target.value)}>
          <option value="">All</option>
          {kinds.map((k) => (
            <option key={k} value={k}>
              {k}
            </option>
          ))}
        </select>
        {state === 'error' && (
          <p className="set-sub">
            The log didn't load.{' '}
            <button className="set-link" onClick={() => fetchPage(kind)}>
              Retry
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
        {state === 'ready' && rows.length === 0 && <p className="set-sub">Nothing logged yet.</p>}
        <div className="set-acts">
          <button
            className="set-link"
            disabled={state === 'loading' || !last}
            onClick={() => last && fetchPage(kind, last.id)}
          >
            Older
          </button>
          <button
            className="set-link"
            disabled={state === 'loading'}
            onClick={() => fetchPage(kind)}
          >
            Refresh
          </button>
        </div>
      </div>
    </Group>
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
    <Group head="INSPECT">
      <div className="set-fold-body">
        <select
          aria-label="User"
          value={id === null ? '' : String(id)}
          onChange={(e) => setId(e.target.value ? Number(e.target.value) : null)}
        >
          <option value="">Pick a user</option>
          {users.map((u) => (
            <option key={u.id} value={u.id}>
              {u.username}
            </option>
          ))}
        </select>
        {data === 'error' && <p className="set-sub">That user didn't load.</p>}
      </div>

      {loaded && id !== null && (
        <>
          <FoldRow label="Config" open={open === 'config'} onToggle={fold('config')}>
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
            label="Tasks"
            value={String(loaded.tasks.length)}
            open={open === 'tasks'}
            onToggle={fold('tasks')}
          >
            {open === 'tasks' && (
              <div className="set-fold-body">
                <div className="log-scroll">
                  <table className="log-table admin-table">
                    <thead>
                      <tr>
                        <th>id</th>
                        <th>title</th>
                        <th>state</th>
                        <th>now</th>
                      </tr>
                    </thead>
                    <tbody>
                      {loaded.tasks.flatMap((t) => [t, ...t.children]).map((t) => (
                        <tr key={t.id}>
                          <td className="mono">{t.id}</td>
                          <td>{t.title}</td>
                          <td className="set-sub">{t.state}</td>
                          <td className="set-sub">{t.is_now ? 'yes' : ''}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>
            )}
          </FoldRow>

          <FoldRow
            label="Today"
            value={String(loaded.events_today.length)}
            open={open === 'today'}
            onToggle={fold('today')}
          >
            {open === 'today' && (
              <div className="set-fold-body">
                <div className="log-scroll">
                  <table className="log-table admin-table">
                    <thead>
                      <tr>
                        <th>time</th>
                        <th>kind</th>
                        <th>status</th>
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
            label="Conversations"
            value={String(loaded.conversations.length)}
            open={open === 'conversations'}
            onToggle={fold('conversations')}
          >
            {open === 'conversations' && (
              <ConversationsFold id={id} items={loaded.conversations} expire={expire} />
            )}
          </FoldRow>

          <FoldRow
            label="Memory"
            value={String(loaded.memory.length)}
            open={open === 'memory'}
            onToggle={fold('memory')}
          >
            {open === 'memory' && (
              <MemoryFold id={id} items={loaded.memory} expire={expire} />
            )}
          </FoldRow>
        </>
      )}

      <FoldRow label="SQL" open={open === 'sql'} onToggle={fold('sql')}>
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
      setSave({ kind: 'failed', message: message(err, "That didn't save. Try again.") })
    }
  }

  return (
    <div className="set-fold-body">
      <span className="set-sub">{path}</span>
      <textarea
        className="mono set-prompt"
        aria-label="User config"
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
          Save
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
              {c.title || `Conversation ${c.id}`}
            </button>
          </li>
        ))}
      </ul>
      {messages === 'error' && <p className="set-sub">Those messages didn't load.</p>}
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
        setSave({ kind: 'failed', message: "That fact didn't load." })
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
      setSave({ kind: 'failed', message: message(err, "That didn't save. Try again.") })
    }
  }

  return (
    <div className="set-fold-body">
      <ul className="admin-list">
        {items.map((m) => (
          <li key={m.id}>
            <button className="set-link" aria-current={mid === m.id} onClick={() => openOne(m.id)}>
              {m.id} · {m.category} · {m.summary}
              {m.archived ? ' · archived' : ''}
            </button>
          </li>
        ))}
      </ul>
      {mid && (
        <>
          <textarea
            className="mono set-prompt"
            aria-label={`Memory ${mid}`}
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
              Save
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
      setError(message(err, "That didn't run."))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="set-fold-body">
      <textarea
        className="mono set-prompt"
        aria-label="SQL"
        rows={5}
        spellCheck={false}
        value={sql}
        onChange={(e) => setSql(e.target.value)}
      />
      <div className="set-acts">
        <button className="btn-haze small" disabled={busy || !sql.trim()} onClick={() => void run()}>
          Run
        </button>
      </div>
      {error && (
        <p className="pane-status bad" role="alert">
          {error}
        </p>
      )}
      {result && 'changes' in result && (
        <p className="set-sub">{result.changes} rows changed</p>
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
          {result.truncated && <p className="set-sub">Truncated at 500 rows.</p>}
        </>
      )}
    </div>
  )
}
