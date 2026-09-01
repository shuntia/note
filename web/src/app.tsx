import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react'
import { api, ApiError, setOnUnauthorized } from './api'
import type { Me } from './types'
import { Memory } from './views/Memory'
import { Settings } from './views/Settings'
import { Talk } from './views/Talk'
import { Tasks } from './views/Tasks'
import { Today } from './views/Today'
import { connectEvents } from './ws'

type Tab = 'today' | 'tasks' | 'chat' | 'memory' | 'settings'

export type ToastAction = { label: string; run: () => void }

// `refresh` is a counter views key on or depend on to refetch; `onChanged` bumps it.
export type ViewProps = {
  notify: (msg: string, action?: ToastAction) => void
  refresh: number
  onChanged: () => void
}

const NAV: { id: Tab; label: string }[] = [
  { id: 'today', label: 'Today' },
  { id: 'tasks', label: 'Tasks' },
  { id: 'chat', label: 'Chat' },
  { id: 'memory', label: 'Memory' },
  { id: 'settings', label: 'Settings' },
]

export function App() {
  // undefined = session check in flight; null = signed out
  const [me, setMe] = useState<Me | null | undefined>(undefined)
  const [tab, setTab] = useState<Tab>('today')
  const [toast, setToast] = useState<{ msg: string; action?: ToastAction } | null>(null)
  const [refresh, setRefresh] = useState(0)

  const toastTimer = useRef(0)
  // An undo toast has to outlast the window its action holds itself open for.
  const notify = useCallback((msg: string, action?: ToastAction) => {
    setToast({ msg, action })
    window.clearTimeout(toastTimer.current)
    toastTimer.current = window.setTimeout(() => setToast(null), action ? 5000 : 4000)
  }, [])

  const onChanged = useCallback(() => setRefresh((n) => n + 1), [])

  useEffect(() => {
    setOnUnauthorized(() => setMe(null))
  }, [])

  useEffect(() => {
    api
      .me()
      .then(setMe)
      .catch(() => setMe(null))
  }, [])

  useEffect(() => {
    if (!me) return
    return connectEvents((ev) => {
      notify(ev.body ? `${ev.title} — ${ev.body}` : ev.title)
      onChanged()
    })
  }, [me, notify, onChanged])

  if (me === undefined) return null
  if (me === null) return <Login onSignedIn={setMe} />

  const views: ViewProps = { notify, refresh, onChanged }

  return (
    <div className="shell">
      <aside className="sidebar">
        <header className="sidebar-head">
          <h1 className="brand">
            <span className="brand-glyph" aria-hidden="true" />
            Note
          </h1>
          <p className="sidebar-sub">
            {me.username} · {new Date().toDateString()}
          </p>
        </header>
        <nav className="sidebar-nav" aria-label="Views">
          {NAV.map((t) => (
            <button
              key={t.id}
              className="sidebar-item"
              aria-current={tab === t.id}
              onClick={() => setTab(t.id)}
            >
              {t.label}
            </button>
          ))}
        </nav>
      </aside>
      <div className="content">
        <header className="mobile-head">
          <div className="brand">
            <span className="brand-glyph" aria-hidden="true" />
            Note
          </div>
          <span className="mobile-sub">{me.username}</span>
        </header>
        <main className={tab === 'chat' ? 'view view-talk' : 'view'}>
          {tab === 'today' && <Today key={refresh} {...views} />}
          {tab === 'tasks' && <Tasks {...views} />}
          {tab === 'chat' && <Talk />}
          {tab === 'memory' && <Memory {...views} />}
          {tab === 'settings' && <Settings me={me} {...views} onSignedOut={() => setMe(null)} />}
        </main>
      </div>
      <nav className="tabs" aria-label="Views">
        {NAV.map((t) => (
          <button key={t.id} aria-current={tab === t.id} onClick={() => setTab(t.id)}>
            {t.label}
          </button>
        ))}
      </nav>
      {toast && (
        <div className="toast" role="status">
          <span className="toast-msg">{toast.msg}</span>
          {toast.action && (
            <button
              className="toast-action"
              onClick={() => {
                toast.action?.run()
                setToast(null)
              }}
            >
              {toast.action.label}
            </button>
          )}
        </div>
      )}
    </div>
  )
}

function Login({ onSignedIn }: { onSignedIn: (me: Me) => void }) {
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setError(null)
    try {
      await api.login(username, password)
      onSignedIn(await api.me())
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) {
        setError('Wrong username or password.')
      } else if (err instanceof ApiError && err.status === 429) {
        setError('Too many attempts. Wait a few minutes.')
      } else {
        setError("Couldn't sign in. Try again.")
      }
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="login">
      <h1>Note</h1>
      <form onSubmit={submit}>
        <div className="field">
          <input
            placeholder="Username"
            autoComplete="username"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
          />
        </div>
        <div className="field">
          <input
            type="password"
            placeholder="Password"
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </div>
        {error && <p role="alert">{error}</p>}
        <button className="primary" disabled={busy || !username || !password}>
          Sign in
        </button>
      </form>
    </div>
  )
}
