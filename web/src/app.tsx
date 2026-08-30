import { useCallback, useEffect, useState, type FormEvent } from 'react'
import { api, ApiError } from './api'
import type { Me } from './types'

type Tab = 'today' | 'tasks' | 'talk' | 'more'

// `refresh` is a counter views key or depend on to refetch; `onChanged` bumps it.
export type ViewProps = {
  notify: (msg: string) => void
  refresh: number
  onChanged: () => void
}

const TABS: { id: Tab; label: string }[] = [
  { id: 'today', label: 'Today' },
  { id: 'tasks', label: 'Tasks' },
  { id: 'talk', label: 'Talk' },
  { id: 'more', label: 'More' },
]

export function App() {
  // undefined = session check in flight; null = signed out
  const [me, setMe] = useState<Me | null | undefined>(undefined)
  const [tab, setTab] = useState<Tab>('today')
  const [toast, setToast] = useState<string | null>(null)
  const [refresh, setRefresh] = useState(0)

  const notify = useCallback((msg: string) => {
    setToast(msg)
    window.setTimeout(() => setToast(null), 4000)
  }, [])

  const onChanged = useCallback(() => setRefresh((n) => n + 1), [])

  useEffect(() => {
    api
      .me()
      .then(setMe)
      .catch(() => setMe(null))
  }, [])

  if (me === undefined) return null
  if (me === null) return <Login onSignedIn={setMe} />

  const views: ViewProps = { notify, refresh, onChanged }

  return (
    <div className="shell">
      <header className="masthead">
        <h1>Note</h1>
        <div className="date">
          {me.username} · {new Date().toDateString()}
        </div>
      </header>
      <main className="view">
        {tab === 'today' && <Placeholder key={refresh} name="Today" {...views} />}
        {tab === 'tasks' && <Placeholder name="Tasks" {...views} />}
        {tab === 'talk' && <Placeholder name="Talk" {...views} />}
        {tab === 'more' && <Placeholder name="More" {...views} />}
      </main>
      <nav className="tabs">
        {TABS.map((t) => (
          <button key={t.id} aria-current={tab === t.id} onClick={() => setTab(t.id)}>
            {t.label}
          </button>
        ))}
      </nav>
      {toast && <div className="toast" role="status">{toast}</div>}
    </div>
  )
}

function Placeholder({ name }: ViewProps & { name: string }) {
  return <p className="muted">{name} is on its way.</p>
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
