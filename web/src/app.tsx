import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react'
import { api, ApiError, setOnUnauthorized } from './api'
import { readSession, writeSession, type FocusSession } from './session'
import type { Me } from './types'
import { Memory } from './views/Memory'
import { Now } from './views/Now'
import { Settings } from './views/Settings'
import { Talk } from './views/Talk'
import { Tasks } from './views/Tasks'
import { Today } from './views/Today'
import { connectEvents } from './ws'

type Tab = 'today' | 'tasks' | 'chat' | 'memory' | 'settings'

const DRAFT_KEY = 'note.captureDraft'
const CAPTURE_PLACEHOLDER = 'Jot anything — a task, a thought, a change of plan'
const UNDO_MS = 5000

// No API removes a task, so the create waits out the undo window before it is sent.
let heldCapture: { title: string; timer: number } | null = null

// `windowMs` is the undo window the action holds open; the toast must outlast it.
export type ToastAction = { label: string; run: () => void; windowMs?: number }

// `refresh` is a counter views key on or depend on to refetch; `onChanged` bumps it.
export type ViewProps = {
  notify: (msg: string, action?: ToastAction) => void
  refresh: number
  onChanged: () => void
  // Switches to Talk on a new conversation with this text waiting in the composer.
  openTalk: (draft: string) => void
  // Hands the shell over to the Now screen for the length of a focus session.
  openNow: (session: FocusSession) => void
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
  const [talkPrefill, setTalkPrefill] = useState<string | null>(null)
  // Reading storage at mount is what makes the Now screen the resting face after a reload.
  const [session, setSession] = useState<FocusSession | null>(readSession)

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

  const changeSession = useCallback((next: FocusSession | null) => {
    setSession(next)
    writeSession(next)
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
    return connectEvents((ev) => {
      notify(ev.body ? `${ev.title} — ${ev.body}` : ev.title)
      onChanged()
    })
  }, [me, notify, onChanged])

  if (me === undefined) return null
  if (me === null) return <Login onSignedIn={setMe} />

  const views: ViewProps = { notify, refresh, onChanged, openTalk, openNow: changeSession }

  const toastNode = toast && (
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
  )

  // The Now screen is a full-bleed focus surface: none of the shell's chrome
  // mounts around it, so its keys never contend with the capture shortcut.
  if (session) {
    return (
      <>
        <Now
          session={session}
          setSession={changeSession}
          notify={notify}
          onChanged={onChanged}
          onLeave={() => setTab('today')}
        />
        {toastNode}
      </>
    )
  }

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
        <Capture notify={notify} onChanged={onChanged} />
        <main className={tab === 'chat' ? 'view view-talk' : 'view'}>
          {tab === 'today' && <Today key={refresh} {...views} />}
          {tab === 'tasks' && <Tasks {...views} />}
          {tab === 'chat' && (
            <Talk {...views} prefill={talkPrefill} onPrefilled={() => setTalkPrefill(null)} />
          )}
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
      {toastNode}
    </div>
  )
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

  const commit = useCallback(() => {
    if (!heldCapture) return
    const { title, timer } = heldCapture
    heldCapture = null
    window.clearTimeout(timer)
    api
      .addTask(title)
      .then(onChanged)
      .catch(() => notify("Couldn't save that. Try again."))
  }, [notify, onChanged])

  useEffect(() => commit, [commit])

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
    commit()
    heldCapture = { title, timer: window.setTimeout(commit, UNDO_MS) }
    notify('Saved to Tasks', {
      label: 'Undo',
      run: () => {
        if (heldCapture?.title !== title) return
        window.clearTimeout(heldCapture.timer)
        heldCapture = null
      },
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
