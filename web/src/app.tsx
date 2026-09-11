import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react'
import { api, ApiError, setOnUnauthorized } from './api'
import { NavIcon } from './navicon'
import { prefsFrom, writePrefs } from './prefs'
import { makeHold } from './held'
import { readSession, writeSession, type FocusSession } from './session'
import type { Me } from './types'
import { Admin } from './views/Admin'
import { Home } from './views/Home'
import { Memory } from './views/Memory'
import { Settings } from './views/Settings'
import { Talk } from './views/Talk'
import { Tasks } from './views/Tasks'
import { Today } from './views/Today'
import { connectEvents } from './ws'

// `admin` is reached from Settings only, so it never joins NAV.
type Tab = 'today' | 'tasks' | 'chat' | 'memory' | 'settings' | 'admin'

const DRAFT_KEY = 'note.captureDraft'
const CAPTURE_PLACEHOLDER = 'Jot anything'

// No API removes a task, so the create waits out the undo window before it is sent.
const captureHold = makeHold<string>()

// `windowMs` is the undo window the action holds open; the toast must outlast it.
export type ToastAction = { label: string; run: () => void; windowMs?: number }

// `refresh` is a counter views key on or depend on to refetch; `onChanged` bumps it.
export type ViewProps = {
  notify: (msg: string, action?: ToastAction) => void
  refresh: number
  onChanged: () => void
  // Switches to Talk on a new conversation with this text waiting in the composer.
  openTalk: (draft: string) => void
  // Opens a focus session; Home carries it for as long as it runs.
  openNow: (session: FocusSession) => void
}

type NavTab = Exclude<Tab, 'admin'>

const NAV: { id: NavTab; label: string }[] = [
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
  const [talkPrefill, setTalkPrefill] = useState<string | null>(null)
  // Reading storage at mount is what makes the session face survive a reload.
  const [session, setSession] = useState<FocusSession | null>(readSession)
  const [chromeHidden, setChromeHidden] = useState(false)
  const mobile = useMedia('(max-width: 767.98px)')

  const toastTimer = useRef(0)
  const notify = useCallback((msg: string, action?: ToastAction) => {
    setToast({ msg, action })
    window.clearTimeout(toastTimer.current)
    toastTimer.current = window.setTimeout(
      () => setToast(null),
      action ? (action.windowMs ?? 5000) : 4000,
    )
  }, [])

  const onChanged = useCallback(() => setRefresh((n) => n + 1), [])

  const openTalk = useCallback((draft: string) => {
    setTalkPrefill(draft)
    setTab('chat')
  }, [])

  // A session takes the screen from wherever it was started, so Today comes with it.
  const changeSession = useCallback((next: FocusSession | null) => {
    setSession(next)
    writeSession(next)
    if (next) setTab('today')
  }, [])

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
    api
      .settings()
      .then((s) => writePrefs(prefsFrom(s)))
      .catch(() => {})
  }, [me])

  useEffect(() => {
    if (!me) return
    return connectEvents((ev) => {
      notify(ev.body ? `${ev.title} — ${ev.body}` : ev.title)
      onChanged()
    })
  }, [me, notify, onChanged])

  if (me === undefined) return null
  if (me === null) return <Login onSignedIn={setMe} />

  const views: ViewProps = { notify, refresh, onChanged, openTalk, openNow: changeSession }

  const toastNode = toast && (
    <div className={`toast${mobile && !chromeHidden ? ' above-tabs' : ''}`} role="status">
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
  )

  const current: NavTab = tab === 'admin' ? 'settings' : tab

  const tabsNode = (
    <nav className="tabs" aria-label="Views">
      {NAV.map((t) => (
        <button key={t.id} aria-current={current === t.id} onClick={() => setTab(t.id)}>
          <NavIcon id={t.id} />
          {t.label}
        </button>
      ))}
    </nav>
  )

  const home = (
    <Home
      session={session}
      setSession={changeSession}
      notify={notify}
      onChanged={onChanged}
      refresh={refresh}
      openNow={changeSession}
      mobile={mobile}
      onChrome={setChromeHidden}
      tabs={tabsNode}
    />
  )

  const showHome = tab === 'today' && (mobile || session !== null)

  return (
    <div className="shell">
      {!mobile && !(showHome && chromeHidden) && (
        <header className="topbar">
          <span className="brand">Note</span>
          <nav className="topnav" aria-label="Views">
            {NAV.map((t) => (
              <button key={t.id} aria-current={current === t.id} onClick={() => setTab(t.id)}>
                {t.label}
              </button>
            ))}
          </nav>
          <Capture notify={notify} onChanged={onChanged} />
        </header>
      )}
      <main className={`view${tab === 'chat' ? ' view-talk' : ''}${showHome ? ' view-home' : ''}`}>
        {showHome && home}
        {tab === 'today' && !showHome && <Today {...views} />}
        {tab === 'tasks' && <Tasks {...views} />}
        {tab === 'chat' && (
          <Talk {...views} prefill={talkPrefill} onPrefilled={() => setTalkPrefill(null)} />
        )}
        {tab === 'memory' && <Memory {...views} />}
        {tab === 'settings' && (
          <Settings
            me={me}
            {...views}
            onSignedOut={() => setMe(null)}
            openAdmin={() => setTab('admin')}
          />
        )}
        {tab === 'admin' && (
          <Admin me={me} notify={notify} onBack={() => setTab('settings')} />
        )}
      </main>
      {mobile && !showHome && tabsNode}
      {toastNode}
    </div>
  )
}

function useMedia(query: string): boolean {
  const [matches, setMatches] = useState(() => window.matchMedia(query).matches)
  useEffect(() => {
    const mq = window.matchMedia(query)
    const on = () => setMatches(mq.matches)
    mq.addEventListener('change', on)
    return () => mq.removeEventListener('change', on)
  }, [query])
  return matches
}

function readDraft(): string {
  try {
    return localStorage.getItem(DRAFT_KEY) ?? ''
  } catch {
    return ''
  }
}

function writeDraft(text: string) {
  try {
    if (text) localStorage.setItem(DRAFT_KEY, text)
    else localStorage.removeItem(DRAFT_KEY)
  } catch {
    // storage blocked; the draft still holds for this session
  }
}

function Capture({
  notify,
  onChanged,
}: {
  notify: (msg: string, action?: ToastAction) => void
  onChanged: () => void
}) {
  const [text, setText] = useState(readDraft)
  const input = useRef<HTMLInputElement>(null)
  // Where focus was when the shortcut stole it, so Esc can hand it back.
  const returnTo = useRef<HTMLElement | null>(null)

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'n' || e.ctrlKey || e.metaKey || e.altKey || e.defaultPrevented) return
      const el = e.target as HTMLElement | null
      const tag = el?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || el?.isContentEditable) return
      e.preventDefault()
      returnTo.current = el
      input.current?.focus()
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [])

  const change = (value: string) => {
    setText(value)
    writeDraft(value)
  }

  const submit = (e: FormEvent) => {
    e.preventDefault()
    const title = text.trim()
    if (!title) return
    change('')
    captureHold.start(title, () => {
      api
        .addTask(title)
        .then(onChanged)
        .catch(() => notify("Couldn't save that. Try again."))
    })
    notify('Saved to Tasks', {
      label: 'Undo',
      run: () => captureHold.cancel(title),
    })
  }

  return (
    <form className="capture" onSubmit={submit}>
      <span className="capture-glyph" aria-hidden="true">
        +
      </span>
      <input
        ref={input}
        value={text}
        aria-label={CAPTURE_PLACEHOLDER}
        placeholder={CAPTURE_PLACEHOLDER}
        onChange={(e) => change(e.target.value)}
        onKeyDown={(e) => {
          if (e.key !== 'Escape') return
          e.currentTarget.blur()
          returnTo.current?.focus()
          returnTo.current = null
        }}
      />
      <kbd className="capture-key" aria-hidden="true">
        N
      </kbd>
    </form>
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
