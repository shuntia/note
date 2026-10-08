import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from 'react'
import { flushSync } from 'react-dom'
import { api, ApiError, setOnUnauthorized } from './api'
import { CallView, type CallOpen } from './call/CallView'
import { primeAudio } from './call/prime'
import { trailing } from './coalesce'
import { notice } from './call/tones'
import { desktop, ringable } from './desktop'
import { Jot } from './jot'
import { glide, lift, viewIn, viewOut } from './motion-gsap'
import { reducedMotion } from './motion'
import { NavIcon } from './navicon'
import { prefsFrom, writePrefs } from './prefs'
import { startPresence } from './presence'
import { readSession, stillEnding, writeSession, type FocusSession } from './session'
import type { Me, SessionStart } from './types'
import { Admin } from './views/Admin'
import { Home } from './views/Home'
import { Memory } from './views/Memory'
import { Onboarding } from './views/Onboarding'
import { Settings } from './views/Settings'
import { Talk } from './views/Talk'
import { Tasks } from './views/Tasks'
import { connectEvents, onCallFrame } from './ws'
import { deviceZone, zoneChange } from './zone'
import './styles/shell.css'
import { adoptLanguage, t, type Key } from './i18n'

// `admin` is reached from Settings only, so it never joins NAV.
type Tab = 'today' | 'tasks' | 'chat' | 'memory' | 'settings' | 'admin'

// The deep link a push notification carries; the id of the thread it names.
function conversationOf(url: string): number | null {
  const hash = url.startsWith('#') ? url : new URL(url, location.href).hash
  const match = /^#\/chat\/(\d+)$/.exec(hash)
  return match ? Number(match[1]) : null
}

// `windowMs` is the undo window the action holds open; the toast must outlast it.
export type ToastAction = { label: string; run: () => void; windowMs?: number }

// `refresh` is a counter views key on or depend on to refetch; `onChanged` bumps it.
export type ViewProps = {
  notify: (msg: string, action?: ToastAction) => void
  refresh: number
  onChanged: () => void
  // Switches to Talk on a new conversation with this text waiting in the composer.
  openTalk: (draft: string) => void
  // Switches to Talk landing on a thread that already exists.
  openConversation: (id: number) => void
  // Opens a focus session on the server and lands on Home, which paints it.
  openNow: (fields: SessionStart) => void
}

type NavTab = Exclude<Tab, 'admin'>

const TOAST_LEAVE_MS = 200

// On a phone Today sits in the middle of the bar, under the thumb.
const PHONE_ORDER: NavTab[] = ['tasks', 'chat', 'today', 'memory', 'settings']

const NAV: { id: NavTab; label: Key }[] = [
  { id: 'today', label: 'nav.today' },
  { id: 'tasks', label: 'nav.notes' },
  { id: 'chat', label: 'nav.chat' },
  { id: 'memory', label: 'nav.memory' },
  { id: 'settings', label: 'nav.settings' },
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
  const [toast, setToast] = useState<{ msg: string; action?: ToastAction; leaving?: boolean } | null>(null)
  const [refresh, setRefresh] = useState(0)
  const [talkPrefill, setTalkPrefill] = useState<string | null>(null)
  // A thread to land on, with a nonce so the same thread can be asked for twice.
  const [talkOpen, setTalkOpen] = useState<{ id: number; at: number } | null>(null)
  // The cache is what the first paint draws; the server answers a moment later.
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
  const dismissToast = useCallback(() => {
    window.clearTimeout(toastTimer.current)
    setToast((t) => t && { ...t, leaving: true })
    toastTimer.current = window.setTimeout(() => setToast(null), TOAST_LEAVE_MS)
  }, [])
  const notify = useCallback(
    (msg: string, action?: ToastAction) => {
      setToast({ msg, action })
      window.clearTimeout(toastTimer.current)
      toastTimer.current = window.setTimeout(dismissToast, action ? (action.windowMs ?? 5000) : 4000)
    },
    [dismissToast],
  )

  const onChanged = useMemo(() => trailing(() => setRefresh((n) => n + 1), 80), [])

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

  const [call, setCall] = useState<CallOpen | null>(null)
  const [micBlocked, setMicBlocked] = useState(false)
  const startCall = useCallback((conversationId: number | null) => {
    primeAudio()
    setCall({ conversationId, ring: null, at: Date.now() })
  }, [])
  // The thread the call talked in opens with its transcript.
  const endCall = useCallback(
    (conversationId: number | null) => {
      setCall(null)
      if (conversationId !== null) openConversation(conversationId)
      else onChanged()
    },
    [openConversation, onChanged],
  )
  const blockMic = useCallback(() => setMicBlocked(true), [])

  useEffect(() => {
    if (!me?.voice) return
    return onCallFrame((f) => {
      if (f.type !== 'incoming' || !ringable()) return
      desktop()?.ring()
      setCall((c) => c ?? { conversationId: f.conversation_id, ring: f.ring, at: Date.now() })
    })
  }, [me])

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

  const putSession = useCallback((s: FocusSession | null) => {
    setSession(s)
    writeSession(s)
  }, [])

  // `/api/sessions/open` is the truth: on load, on everything the server says it
  // decided, and whenever the tab is looked at again.
  const syncSession = useCallback(() => {
    api
      .openWorkSession()
      .then((open) => {
        if (open && stillEnding(open.id)) return
        putSession(open)
      })
      .catch(() => {})
  }, [putSession])

  useEffect(() => {
    if (!me) return
    syncSession()
  }, [me, refresh, syncSession])

  useEffect(() => {
    if (!me) return
    window.addEventListener('focus', syncSession)
    return () => window.removeEventListener('focus', syncSession)
  }, [me, syncSession])

  // A session takes the screen from wherever it was started, so Today comes with it.
  const startSession = useCallback(
    async (fields: SessionStart) => {
      go('today')
      try {
        putSession(await api.startWorkSession(fields))
      } catch {
        notify(t('session.startFailed'))
      }
    },
    [go, notify, putSession],
  )

  // The phone's bar rests on Today once the hand has been still a while; any touch
  // anywhere brings it back. In a session Today's own idle takes the bar away instead.
  const [rested, setRested] = useState(false)
  const inSession = session !== null
  useEffect(() => {
    setRested(false)
    if (!mobile || tab !== 'today' || inSession) return
    let timer = 0
    const wake = () => {
      setRested(false)
      window.clearTimeout(timer)
      timer = window.setTimeout(() => setRested(true), 4000)
    }
    wake()
    document.addEventListener('pointerdown', wake)
    return () => {
      window.clearTimeout(timer)
      document.removeEventListener('pointerdown', wake)
    }
  }, [mobile, tab, inSession])

  // While the phone's keyboard is up for the chat composer, the bar steps aside.
  const [typing, setTyping] = useState(false)
  useEffect(() => {
    if (!mobile) return
    const composing = (el: EventTarget | null) => el instanceof HTMLTextAreaElement && el.closest('.chat-compose') !== null
    const onIn = (e: FocusEvent) => setTyping(composing(e.target))
    const onOut = (e: FocusEvent) => setTyping(composing(e.relatedTarget))
    setTyping(composing(document.activeElement))
    document.addEventListener('focusin', onIn)
    document.addEventListener('focusout', onOut)
    return () => {
      document.removeEventListener('focusin', onIn)
      document.removeEventListener('focusout', onOut)
    }
  }, [mobile])

  // Desktop rests the top bar and its jot for as long as a session runs.
  useEffect(() => {
    document.documentElement.classList.toggle('in-session', session !== null)
  }, [session])

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
      .then((s) => {
        writePrefs(prefsFrom(s))
        if (adoptLanguage(s.language)) location.reload()
      })
      .catch(() => {})
  }, [me])

  useEffect(() => {
    if (!me || me.onboarding) return
    let busy = false
    const check = async () => {
      if (busy) return
      busy = true
      try {
        const change = zoneChange(await api.settings(), deviceZone())
        if (!change) return
        await api.saveSettings({ timezone: change.to })
        onChanged()
        notify(t('zone.follows', { zone: change.to.replace(/_/g, ' ') }), {
          label: t('toast.undo'),
          windowMs: 8000,
          run: () =>
            void api
              .saveSettings({ timezone: change.from, timezone_auto: false })
              .then(onChanged, () => notify(t('toast.undoFailed'))),
        })
      } catch {
        // The next focus tries again.
      } finally {
        busy = false
      }
    }
    void check()
    window.addEventListener('focus', check)
    return () => window.removeEventListener('focus', check)
  }, [me, notify, onChanged])

  useEffect(() => {
    if (!me) return
    return startPresence({
      ping: () => void api.presence().catch(() => {}),
      target: window,
      visible: () => !document.hidden,
      now: Date.now,
    })
  }, [me])

  useEffect(() => {
    if (!me) return
    return connectEvents((ev) => {
      if (desktop()) notice()
      // A check-in (it carries its event) or an urgent ask has its question waiting in
      // the thread, so the thread is the notice; a session's notices are toasts over
      // whatever is open.
      if (ev.conversation_id !== null && (ev.event_id !== null || ev.urgency === 'high')) openConversation(ev.conversation_id)
      else notify(ev.body ? `${ev.title} — ${ev.body}` : ev.title)
      onChanged()
    }, onChanged)
  }, [me, notify, onChanged, openConversation])

  if (me === undefined) return null
  if (me === null) return <Login onSignedIn={setMe} />
  if (me.onboarding)
    return (
      <>
        <Onboarding me={me} notify={notify} onDone={() => setMe({ ...me, onboarding: false })} />
        {toast && (
          <div className={`toast${toast.leaving ? ' leaving' : ''}`} role="status">
            <span className="toast-msg">{toast.msg}</span>
          </div>
        )}
      </>
    )

  const views: ViewProps = {
    notify,
    refresh,
    onChanged,
    openTalk,
    openConversation,
    openNow: (fields: SessionStart) => void startSession(fields),
  }

  const toastNode = toast && (
    <div className={`toast${mobile ? ' above-tabs' : ''}${toast.leaving ? ' leaving' : ''}`} role="status">
      <span className="toast-msg">{toast.msg}</span>
      {toast.action && (
        <button
          className="toast-action"
          onClick={() => {
            toast.action?.run()
            dismissToast()
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
          setSession={putSession}
          notify={notify}
          onChanged={onChanged}
          refresh={refresh}
          openNow={views.openNow}
          openTalk={openTalk}
          openConversation={openConversation}
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
          onCall={me.voice ? startCall : undefined}
          micBlocked={micBlocked}
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
          <span className="brand">{t('app.name')}</span>
          <Rail kind="topnav" current={current} go={go} />
          <Jot openTalk={openTalk} openConversation={openConversation} tab={tab} />
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
      {mobile && <Rail kind="tabs" current={current} go={go} away={rested || typing} />}
      {mobile && <div className={`handle${rested ? ' on' : ''}`} aria-hidden="true" />}
      {toastNode}
      {call && <CallView key={call.at} call={call} onClose={endCall} onMicBlocked={blockMic} />}
    </div>
  )
}

// The bar of views, with the mark behind the current one travelling to whichever is
// chosen next; `tabs` carries icons and sits at the foot of a phone.
function Rail({
  kind,
  current,
  go,
  away = false,
}: {
  kind: 'topnav' | 'tabs'
  current: NavTab
  go: (t: NavTab) => void
  away?: boolean
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
    <nav ref={nav} className={`${kind}${away ? ' away' : ''}`} aria-label={t('nav.views')}>
      <span className="nav-glide" aria-hidden="true" ref={mark} />
      {kind === 'tabs'
        ? PHONE_ORDER.map((id) => NAV.find((item) => item.id === id)!).map((item) => (
            <button key={item.id} aria-current={current === item.id} aria-label={t(item.label)} onClick={() => go(item.id)}>
              <NavIcon id={item.id} />
            </button>
          ))
        : NAV.map((item) => (
            <button key={item.id} aria-current={current === item.id} onClick={() => go(item.id)}>
              {t(item.label)}
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
        setError(t('login.wrong'))
      } else if (err instanceof ApiError && err.status === 429) {
        setError(t('login.throttled'))
      } else {
        setError(t('login.failed'))
      }
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="login">
      <h1>{t('app.name')}</h1>
      <form onSubmit={submit}>
        <div className="field">
          <input
            placeholder={t('login.username')}
            autoComplete="username"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
          />
        </div>
        <div className="field">
          <input
            type="password"
            placeholder={t('login.password')}
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </div>
        {error && <p role="alert">{error}</p>}
        <button className="primary" disabled={busy || !username || !password}>
          {t('login.signIn')}
        </button>
      </form>
    </div>
  )
}
