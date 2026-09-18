import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from 'react'
import { flushSync } from 'react-dom'
import { api, ApiError, setOnUnauthorized } from './api'
import { glide, lift, viewIn, viewOut } from './motion-gsap'
import { reducedMotion } from './motion'
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
import { connectEvents } from './ws'
import './styles/shell.css'

// `admin` is reached from Settings only, so it never joins NAV.
type Tab = 'today' | 'tasks' | 'chat' | 'memory' | 'settings' | 'admin'

const DRAFT_KEY = 'note.captureDraft'
const CAPTURE_PLACEHOLDER = 'Jot anything'

// The deep link a push notification or an ntfy click carries; the id of the thread it names.
function conversationOf(url: string): number | null {
  const hash = url.startsWith('#') ? url : new URL(url, location.href).hash
  const match = /^#\/chat\/(\d+)$/.exec(hash)
  return match ? Number(match[1]) : null
}

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

// A view on screen. A turn keeps the one being replaced alive under its own id until
// it has faded, so nothing in it is torn down mid-transition.
type Layer = { id: number; tab: Tab }

export function App() {
  // undefined = session check in flight; null = signed out
  const [me, setMe] = useState<Me | null | undefined>(undefined)
  const [layer, setLayer] = useState<Layer>({ id: 0, tab: 'today' })
  const [leaving, setLeaving] = useState<Layer | null>(null)
  // true from the moment a turn starts until the arriving layer has settled
  const [turning, setTurning] = useState(false)
  const [toast, setToast] = useState<{ msg: string; action?: ToastAction } | null>(null)
  const [refresh, setRefresh] = useState(0)
  const [talkPrefill, setTalkPrefill] = useState<string | null>(null)
  // A thread to land on, with a nonce so the same thread can be asked for twice.
  const [talkOpen, setTalkOpen] = useState<{ id: number; at: number } | null>(null)
  // Reading storage at mount is what makes the session face survive a reload.
  const [session, setSession] = useState<FocusSession | null>(readSession)
  const mobile = useMedia('(max-width: 767.98px)')

  const tab = layer.tab
  const here = useRef(layer)
  here.current = layer
  const layerEls = useRef(new Map<number, HTMLElement>())
  const turn = useRef(0)

  // Every way into a view goes through here: the outgoing one is held where it is
  // while the arriving one takes the layout, and the page starts at the top again.
  const go = useCallback((next: Tab) => {
    const from = here.current
    if (next === from.tab) return
    const id = turn.current + 1
    turn.current = id
    const el = layerEls.current.get(from.id)
    if (!el || reducedMotion()) {
      setLeaving(null)
      setTurning(false)
      setLayer({ id, tab: next })
      window.scrollTo(0, 0)
      return
    }
    // Home pins the page as it scrolls, and lets go before its layer is lifted. The
    // spacer that pin held leaves the page with it, so the scroll follows it down and
    // what is on screen stays where it was; the reset is then the arriving view's.
    const tall = document.documentElement.scrollHeight
    const y = window.scrollY
    flushSync(() => setTurning(true))
    const shed = tall - document.documentElement.scrollHeight
    if (shed > 0) window.scrollTo(0, Math.max(0, y - shed))
    lift(el)
    window.scrollTo(0, 0)
    setLeaving(from)
    setLayer({ id, tab: next })
  }, [])

  useLayoutEffect(() => {
    if (!leaving) return
    const id = turn.current
    const mine = () => turn.current === id
    viewOut(layerEls.current.get(leaving.id), () => mine() && setLeaving(null))
    viewIn(layerEls.current.get(layer.id), () => mine() && setTurning(false))
  }, [leaving, layer])

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

  const openTalk = useCallback(
    (draft: string) => {
      setTalkPrefill(draft)
      go('chat')
    },
    [go],
  )

  const openConversation = useCallback(
    (id: number) => {
      setTalkOpen({ id, at: Date.now() })
      go('chat')
    },
    [go],
  )

  // `#/chat/<id>` in the address bar, at load or from a notification, and the
  // same route handed over by the service worker when a tab is already open.
  useEffect(() => {
    if (!me) return
    const fromHash = () => {
      const id = conversationOf(location.hash)
      if (id === null) return
      history.replaceState(null, '', location.pathname + location.search)
      openConversation(id)
    }
    const fromWorker = (e: MessageEvent) => {
      const data: unknown = e.data
      if (typeof data !== 'object' || data === null) return
      const { type, url } = data as { type?: unknown; url?: unknown }
      if (type !== 'open' || typeof url !== 'string') return
      const id = conversationOf(url)
      if (id !== null) openConversation(id)
    }
    fromHash()
    window.addEventListener('hashchange', fromHash)
    navigator.serviceWorker?.addEventListener('message', fromWorker)
    return () => {
      window.removeEventListener('hashchange', fromHash)
      navigator.serviceWorker?.removeEventListener('message', fromWorker)
    }
  }, [me, openConversation])

  // A session takes the screen from wherever it was started, so Today comes with it.
  const changeSession = useCallback(
    (next: FocusSession | null) => {
      setSession(next)
      writeSession(next)
      if (next) go('today')
    },
    [go],
  )

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
      // A check-in's question is waiting in its thread, so the thread is the notice.
      if (ev.conversation_id !== null) openConversation(ev.conversation_id)
      else notify(ev.body ? `${ev.title} — ${ev.body}` : ev.title)
      onChanged()
    })
  }, [me, notify, onChanged, openConversation])

  if (me === undefined) return null
  if (me === null) return <Login onSignedIn={setMe} />

  const views: ViewProps = { notify, refresh, onChanged, openTalk, openNow: changeSession }

  const toastNode = toast && (
    <div className={`toast${mobile ? ' above-tabs' : ''}`} role="status">
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

  const viewOf = (of: Tab): ReactNode => {
    if (of === 'today')
      return (
        <Home
          session={session}
          setSession={changeSession}
          notify={notify}
          onChanged={onChanged}
          refresh={refresh}
          openNow={changeSession}
          mobile={mobile}
          armed={!turning}
        />
      )
    if (of === 'tasks') return <Tasks {...views} />
    if (of === 'chat')
      return (
        <Talk
          {...views}
          prefill={talkPrefill}
          onPrefilled={() => setTalkPrefill(null)}
          open={talkOpen}
          onOpened={() => setTalkOpen(null)}
          session={session}
          goHome={() => go('today')}
        />
      )
    if (of === 'memory') return <Memory {...views} />
    if (of === 'settings')
      return (
        <Settings
          me={me}
          {...views}
          onSignedOut={() => setMe(null)}
          openAdmin={() => go('admin')}
        />
      )
    return <Admin me={me} notify={notify} onBack={() => go('settings')} />
  }

  const layers = leaving ? [leaving, layer] : [layer]

  return (
    <div className="shell">
      {!mobile && (
        <header className="topbar">
          <span className="brand">Note</span>
          <Rail kind="topnav" current={current} go={go} />
          <Capture notify={notify} onChanged={onChanged} />
        </header>
      )}
      {layers.map((l) => (
        <main
          key={l.id}
          ref={(el) => {
            if (el) layerEls.current.set(l.id, el)
            else layerEls.current.delete(l.id)
          }}
          className={[
            'view',
            l.tab === 'chat' ? 'view-talk' : '',
            l.tab === 'today' ? 'view-home' : '',
            l === leaving ? 'leaving' : '',
          ]
            .filter(Boolean)
            .join(' ')}
          aria-hidden={l === leaving || undefined}
        >
          {viewOf(l.tab)}
        </main>
      ))}
      {mobile && <Rail kind="tabs" current={current} go={go} />}
      {toastNode}
    </div>
  )
}

// The bar of views, with the mark behind the current one travelling to whichever is
// chosen next; `tabs` carries icons and sits at the foot of a phone.
function Rail({
  kind,
  current,
  go,
}: {
  kind: 'topnav' | 'tabs'
  current: NavTab
  go: (t: NavTab) => void
}) {
  const nav = useRef<HTMLElement>(null)
  const mark = useRef<HTMLSpanElement>(null)
  const placed = useRef(false)

  useLayoutEffect(() => {
    const root = nav.current
    if (!root) return
    const inset = kind === 'tabs' ? 10 : 0
    const put = (animate: boolean) => {
      const on = root.querySelector<HTMLElement>('button[aria-current="true"]')
      if (!on) return
      glide(mark.current, { x: on.offsetLeft + inset, width: on.offsetWidth - inset * 2 }, animate)
    }
    put(placed.current)
    placed.current = true
    const ro = new ResizeObserver(() => put(false))
    ro.observe(root)
    return () => ro.disconnect()
  }, [kind, current])

  return (
    <nav ref={nav} className={kind} aria-label="Views">
      <span className="nav-glide" aria-hidden="true" ref={mark} />
      {NAV.map((t) => (
        <button key={t.id} aria-current={current === t.id} onClick={() => go(t.id)}>
          {kind === 'tabs' && <NavIcon id={t.id} />}
          {t.label}
        </button>
      ))}
    </nav>
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
