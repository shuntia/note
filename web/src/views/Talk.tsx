import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
  type ReactNode,
} from 'react'
import { flushSync } from 'react-dom'
import gsap from 'gsap'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { Markdown } from '../markdown'
import { reducedMotion } from '../motion'
import { Overflow } from '../overflow'
import { Pulse } from '../pulse'
import { doing, receipt } from '../receipts'
import { makeHold } from '../held'
import type { FocusSession } from '../session'
import { onAgentFrame, type AgentFrame } from '../ws'
import type { Conversation, TalkMessage, TalkStep } from '../types'
import '../styles/talk.css'

type Item =
  | { kind: 'user'; key: string; text: string }
  | {
      kind: 'assistant'
      key: string
      text: string
      reasoning: string
      thoughtMs: number | null
      steps: ToolItem[]
    }
  | { kind: 'tool'; key: string; name: string; args: string; result: string; isError: boolean }
  | { kind: 'steps'; key: string; steps: ToolItem[] }
  | { kind: 'system'; key: string; text: string; hint: string | null }

type ToolItem = Extract<Item, { kind: 'tool' }> & { running?: boolean; finishedAt?: number }
type AssistantItem = Extract<Item, { kind: 'assistant' }>
type StepsItem = Extract<Item, { kind: 'steps' }>
type TurnItem = Extract<Item, { kind: 'user' | 'system' }>
type Shown = AssistantItem | StepsItem | TurnItem

// One session's progress as the live frames describe it; `seq` is the last one applied.
type Live = { seq: number; reasoning: string; steps: ToolItem[] }

const EMPTY_LIVE: Live = { seq: -1, reasoning: '', steps: [] }

function applyFrame(prev: Live, frame: AgentFrame): Live {
  if (frame.seq <= prev.seq) return prev
  const at = { ...prev, seq: frame.seq }
  const ev = frame.event
  if (ev.kind === 'thinking')
    return { ...at, reasoning: [at.reasoning, ev.text].filter(Boolean).join('\n\n') }
  if (ev.kind !== 'tool_call' && ev.kind !== 'tool_result') return at
  const steps = at.steps.slice()
  const before = steps[ev.index]
  const call = ev.kind === 'tool_call'
  steps[ev.index] = {
    kind: 'tool',
    key: `live-${ev.index}`,
    name: ev.name,
    args: call ? ev.args : (before?.args ?? ''),
    result: call ? '' : ev.result,
    isError: call ? false : ev.is_error,
    running: call,
    finishedAt: call ? undefined : Date.now(),
  }
  return { ...at, steps }
}

type Load = 'loading' | 'ready' | 'error'

const UNDO_MS = 10_000

// How long a finished call's receipt holds the live status line before it falls back.
const RECEIPT_MS = 2_500

// The send flight, the scroll that reveals it and the composer's settling
// share one easing.
const FLIGHT_S = 0.45
const SETTLE_S = 0.3
const SETTLE_EASE = 'expo.out'

// Deleting a conversation has no server-side reversal, so the request waits out the
// undo window before it is sent.
const deleteHold = makeHold<number>(UNDO_MS)

let sequence = 0
const nextKey = () => `local-${++sequence}`

function fromMessage(m: TalkMessage): Item {
  const key = `msg-${m.id}`
  if (m.role === 'user') return { kind: 'user', key, text: m.content }
  if (m.role === 'assistant')
    return {
      kind: 'assistant',
      key,
      text: m.content,
      reasoning: m.reasoning ?? '',
      thoughtMs: m.thought_ms,
      steps: [],
    }
  return {
    kind: 'tool',
    key,
    name: m.tool_name ?? 'tool',
    args: m.tool_args ?? '',
    result: m.content,
    isError: m.is_error,
  }
}

const fromStep = (s: TalkStep): ToolItem => ({
  kind: 'tool',
  key: nextKey(),
  name: s.name,
  args: s.args,
  result: s.result,
  isError: s.is_error,
})

function pretty(raw: string): string {
  const text = raw.trim()
  if (!text) return ''
  try {
    return JSON.stringify(JSON.parse(text), null, 2)
  } catch {
    return raw
  }
}

function errorText(err: unknown): string {
  if (err instanceof ApiError) return err.message
  return "Couldn't reach Note. Check your connection and try again."
}

function shortDate(iso: string): string {
  const at = new Date(iso)
  if (Number.isNaN(at.getTime())) return ''
  return at.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
}

function Receipt({ item }: { item: ToolItem }) {
  const [open, setOpen] = useState(false)
  const args = pretty(item.args)
  const result = pretty(item.result)
  const state = [item.isError ? 'error' : '', open ? 'open' : ''].filter(Boolean).join(' ')
  return (
    <div className={`receipt ${state}`.trim()}>
      <button className="receipt-chip" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
        <svg
          className={item.running ? 'receipt-mark running' : 'receipt-mark'}
          viewBox="0 0 24 24"
          aria-hidden="true"
        >
          {item.running && <path d="M12 3a9 9 0 0 1 9 9" />}
          {!item.running &&
            (item.isError ? <path d="M6 6l12 12M18 6L6 18" /> : <path d="M5 12.5l4.5 4.5L19 7.5" />)}
        </svg>
        <span className="receipt-text">
          {item.running ? doing(item.name, item.args) : receipt(item.name, item.args, item.isError)}
        </span>
        <svg className="receipt-chev" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M9 6l6 6-6 6" />
        </svg>
      </button>
      {open && (
        <div className="receipt-body">
          <div className="receipt-tool">{item.name}</div>
          {args && <pre className="receipt-block">{args}</pre>}
          <pre className="receipt-block">{result || (item.running ? '…' : '—')}</pre>
        </div>
      )}
    </div>
  )
}

const STEPS_LABEL = "Note's steps"

// The header over a finished reply; null when it would open onto nothing.
function thoughtLabel(item: AssistantItem): string | null {
  if (!item.reasoning && item.steps.length === 0) return null
  if (item.thoughtMs === null) return STEPS_LABEL
  if (item.thoughtMs < 1000) return 'Note thought for a moment'
  const seconds = Math.round(item.thoughtMs / 1000)
  return `Note thought for ${seconds} second${seconds === 1 ? '' : 's'}`
}

const lastRunning = (steps: ToolItem[]): ToolItem | undefined =>
  steps.reduce<ToolItem | undefined>((last, s) => (s.running ? s : last), undefined)

const lastFinished = (steps: ToolItem[]): ToolItem | undefined =>
  steps.reduce<ToolItem | undefined>(
    (best, s) =>
      s.finishedAt && !s.running && (!best?.finishedAt || s.finishedAt >= best.finishedAt)
        ? s
        : best,
    undefined,
  )

// The single line a session in flight shows: the call being processed, or the one that
// just finished until RECEIPT_MS has run out.
function liveLabel(steps: ToolItem[], now: number): string {
  const running = lastRunning(steps)
  if (running) return `Note is thinking… · ${doing(running.name, running.args)}`
  const just = lastFinished(steps)
  if (just?.finishedAt && now - just.finishedAt < RECEIPT_MS)
    return `Note is thinking… · ${receipt(just.name, just.args, just.isError)}`
  return 'Note is thinking…'
}

// One quiet line; the reasoning and the calls stay a chevron away. `held` keeps a
// live line in the layout but out of sight until the sent bubble has landed.
function Trace({
  label,
  reasoning,
  steps,
  live = false,
  held = false,
}: {
  label: string
  reasoning: string
  steps: ToolItem[]
  live?: boolean
  held?: boolean
}) {
  const [open, setOpen] = useState(false)
  const state = [live ? 'live' : '', held ? 'held' : '', open ? 'open' : '']
    .filter(Boolean)
    .join(' ')
  return (
    <div className={`activity ${state}`.trim()}>
      <button className="activity-head" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
        <span className="activity-status" aria-live={live ? 'polite' : undefined}>
          {label}
        </span>
        <svg className="receipt-chev" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M9 6l6 6-6 6" />
        </svg>
      </button>
      {open && (
        <div className="activity-body">
          {reasoning && <pre className="activity-think">{reasoning}</pre>}
          {steps.length > 0 && (
            <div className="receipts">
              {steps.map((step) => (
                <Receipt key={step.key} item={step} />
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  )
}

// The transcript records a turn's calls before the reply that explains them; the
// panel hangs them off that reply instead. Calls nothing answered stand alone.
function grouped(items: Item[]): Shown[] {
  const out: Shown[] = []
  let pending: ToolItem[] = []
  const flush = () => {
    if (!pending.length) return
    out.push({ kind: 'steps', key: pending[0].key, steps: pending })
    pending = []
  }
  for (const item of items) {
    if (item.kind === 'tool') {
      pending.push(item)
      continue
    }
    if (item.kind === 'assistant') {
      out.push({ ...item, steps: [...pending, ...item.steps] })
      pending = []
      continue
    }
    flush()
    out.push(item)
  }
  flush()
  return out
}

// A reply that arrived in this sitting eases in; a loaded transcript is already there.
const fresh = (key: string) => key.startsWith('local-')

function assistantTurn(item: AssistantItem): ReactNode {
  const label = thoughtLabel(item)
  return (
    <div key={item.key} className={`reply${fresh(item.key) ? ' fresh' : ''}`}>
      {label && <Trace label={label} reasoning={item.reasoning} steps={item.steps} />}
      <div className="turn assistant">
        <Markdown text={item.text} />
      </div>
    </div>
  )
}

function turn(item: TurnItem): ReactNode {
  if (item.kind === 'user')
    return (
      <div key={item.key} className="turn user" data-key={item.key}>
        {item.text}
      </div>
    )
  return (
    <div key={item.key} className="turn system" role="alert">
      <span>{item.text}</span>
      {item.hint && <span className="chat-hint">{item.hint}</span>}
    </div>
  )
}

// The composer text becomes the sent bubble: a copy lifts off the composer, takes
// the bubble's colour and shape on the way, and lands where the real one waits.
function flyToBubble(
  text: string,
  from: DOMRect,
  fromStyle: CSSStyleDeclaration,
  bubble: HTMLElement,
  to: { left: number; top: number; width: number },
  onLand: () => void,
) {
  const toStyle = getComputedStyle(bubble)
  const ghost = document.createElement('div')
  ghost.className = 'send-ghost'
  ghost.textContent = text
  Object.assign(ghost.style, {
    left: `${from.left}px`,
    top: `${from.top}px`,
    width: `${from.width}px`,
    padding: fromStyle.padding,
    fontSize: fromStyle.fontSize,
    lineHeight: fromStyle.lineHeight,
  })
  document.body.append(ghost)
  bubble.style.visibility = 'hidden'
  requestAnimationFrame(() => ghost.classList.add('bubble'))
  gsap.to(ghost, {
    left: to.left,
    top: to.top,
    width: to.width,
    padding: toStyle.padding,
    fontSize: toStyle.fontSize,
    lineHeight: toStyle.lineHeight,
    duration: FLIGHT_S,
    ease: SETTLE_EASE,
    onComplete: () => {
      bubble.style.visibility = ''
      ghost.remove()
      onLand()
    },
  })
}

export function Talk({
  notify,
  onChanged,
  refresh,
  prefill,
  onPrefilled,
  open,
  onOpened,
  session = null,
  goHome,
}: ViewProps & {
  prefill?: string | null
  onPrefilled?: () => void
  // A thread to land on, from a check-in or a deep link; `at` tells one ask from the next.
  open?: { id: number; at: number } | null
  onOpened?: () => void
  session?: FocusSession | null
  goHome?: () => void
}) {
  const [conversations, setConversations] = useState<Conversation[]>([])
  const [listState, setListState] = useState<Load>('loading')
  const [current, setCurrent] = useState<number | null>(null)
  const [items, setItems] = useState<Item[]>([])
  const [msgState, setMsgState] = useState<Load>('ready')
  const [draft, setDraft] = useState(prefill ?? '')
  // era of the send in flight, so its pending row belongs to the conversation that sent it
  const [pending, setPending] = useState<number | null>(null)
  const [live, setLive] = useState<Live>(EMPTY_LIVE)
  const [flying, setFlying] = useState(false)
  const [sideOpen, setSideOpen] = useState(false)
  const [renaming, setRenaming] = useState<{ id: number; value: string } | null>(null)
  const [sideNotice, setSideNotice] = useState<string | null>(null)
  const [, tick] = useState(0)

  const pane = useRef<HTMLDivElement>(null)
  const input = useRef<HTMLTextAreaElement>(null)
  const stick = useRef(true)
  // a freshly loaded transcript opens at its end at once; everything after eases there
  const jump = useRef(false)
  const wanted = useRef<number | null>(null)
  // bumped whenever the open conversation changes, so a late reply never lands in the wrong pane
  const era = useRef(0)
  const busy = pending === era.current
  // the send in flight, for the frames arriving on the shell's socket
  const inFlight = useRef<{ conversation: number | null } | null>(null)
  // set while a sent bubble is on its way, so nothing else moves the pane
  const flight = useRef(false)

  const loadList = useCallback(async (quiet = false) => {
    if (!quiet) setListState('loading')
    try {
      setConversations(await api.conversations())
      setListState('ready')
    } catch {
      if (!quiet) setListState('error')
    }
  }, [])

  const loadMessages = useCallback(async (id: number) => {
    wanted.current = id
    setMsgState('loading')
    try {
      const rows = await api.conversationMessages(id)
      if (wanted.current !== id) return
      jump.current = true
      setItems(rows.map(fromMessage))
      setMsgState('ready')
    } catch {
      if (wanted.current !== id) return
      setMsgState('error')
    }
  }, [])

  useEffect(() => {
    void loadList()
  }, [loadList])

  // A brand-new conversation's frames carry no id, so they belong to whatever send is open.
  useEffect(
    () =>
      onAgentFrame((frame) => {
        const sent = inFlight.current
        if (!sent) return
        if (frame.conversation_id !== null && frame.conversation_id !== sent.conversation) return
        setLive((prev) => applyFrame(prev, frame))
      }),
    [],
  )

  // A receipt on the status line expires on the clock alone, with no frame to redraw it.
  useEffect(() => {
    if (!busy || lastRunning(live.steps)) return
    const at = lastFinished(live.steps)?.finishedAt
    if (!at) return
    const left = at + RECEIPT_MS - Date.now()
    if (left <= 0) return
    const timer = window.setTimeout(() => tick((n) => n + 1), left)
    return () => window.clearTimeout(timer)
  }, [busy, live.steps])

  const scrollToEnd = useCallback((instant: boolean) => {
    const el = pane.current
    if (!el) return
    const end = el.scrollHeight - el.clientHeight
    if (instant || reducedMotion()) {
      gsap.killTweensOf(el)
      el.scrollTop = end
      return
    }
    gsap.to(el, { scrollTop: end, duration: SETTLE_S, ease: SETTLE_EASE, overwrite: true })
  }, [])

  useEffect(() => {
    if (flight.current || !stick.current) return
    const instant = jump.current
    jump.current = false
    scrollToEnd(instant)
  }, [items, busy, live, msgState, scrollToEnd])

  const fitComposer = useCallback(() => {
    const el = input.current
    if (!el) return 0
    el.style.height = 'auto'
    const style = window.getComputedStyle(el)
    const line = parseFloat(style.lineHeight) || 22
    const frame =
      parseFloat(style.paddingTop) +
      parseFloat(style.paddingBottom) +
      parseFloat(style.borderTopWidth) +
      parseFloat(style.borderBottomWidth)
    const max = line * 8 + frame
    const height = Math.min(el.scrollHeight, max)
    el.style.height = `${height}px`
    el.style.overflowY = el.scrollHeight > max ? 'auto' : 'hidden'
    return height
  }, [])

  useLayoutEffect(() => {
    fitComposer()
  }, [draft, fitComposer])

  const focusInput = () => {
    const el = input.current
    if (!el) return
    el.focus()
    el.setSelectionRange(el.value.length, el.value.length)
  }

  // Talk mounts fresh on every view switch, so this arrives on a new conversation.
  useEffect(() => {
    if (!prefill) return
    setDraft(prefill)
    focusInput()
    onPrefilled?.()
  }, [prefill, onPrefilled])

  useEffect(() => {
    if (!sideOpen) return
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === 'Escape') setSideOpen(false)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [sideOpen])

  const show = (id: number) => {
    setSideOpen(false)
    setSideNotice(null)
    if (id === current) return
    stick.current = true
    era.current++
    setItems([])
    setCurrent(id)
    void loadMessages(id)
  }

  // A thread asked for from outside lands with its newest message in view and
  // the composer ready; asked for again while open, it reloads.
  useEffect(() => {
    if (!open) return
    stick.current = true
    if (open.id === current) void loadMessages(open.id)
    else show(open.id)
    void loadList(true)
    focusInput()
    onOpened?.()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open?.at])

  const startNew = () => {
    setSideOpen(false)
    setSideNotice(null)
    stick.current = true
    era.current++
    wanted.current = null
    setCurrent(null)
    setItems([])
    setMsgState('ready')
    input.current?.focus()
  }

  // Chat opens on an empty conversation — the drawer's own control — unless a thread
  // was named on the way in. Nothing is written until the first message is sent, so
  // leaving an untouched one behind costs nothing.
  useEffect(() => {
    if (!open) startNew()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const send = async () => {
    const text = draft.trim()
    // a send while history is still loading would be wiped by the load's setItems
    if (!text || busy || msgState === 'loading') return
    const mine: Item = { kind: 'user', key: nextKey(), text }
    const sentIn = era.current
    stick.current = true
    inFlight.current = { conversation: current }

    const el = input.current
    const paneEl = pane.current
    const motion = !!el && !!paneEl && !reducedMotion()
    if (motion) {
      const from = el.getBoundingClientRect()
      const fromStyle = getComputedStyle(el)
      const oldHeight = el.offsetHeight
      flight.current = true
      // committed at once so the bubble's slot can be measured before any paint
      flushSync(() => {
        setLive(EMPTY_LIVE)
        setPending(sentIn)
        setDraft('')
        setFlying(true)
        setItems((prev) => [...prev, mine])
      })
      const bubble = paneEl.querySelector<HTMLElement>(`[data-key="${mine.key}"]`)
      const land = () => {
        flight.current = false
        setFlying(false)
        if (stick.current) scrollToEnd(false)
      }
      if (bubble) {
        // where the bubble will sit once the pane has scrolled to its end
        const end = paneEl.scrollHeight - paneEl.clientHeight
        const rect = bubble.getBoundingClientRect()
        const to = { left: rect.left, top: rect.top - (end - paneEl.scrollTop), width: rect.width }
        gsap.to(paneEl, { scrollTop: end, duration: FLIGHT_S, ease: SETTLE_EASE, overwrite: true })
        flyToBubble(text, from, fromStyle, bubble, to, land)
      } else {
        land()
      }
      const settled = fitComposer()
      el.classList.add('flying')
      gsap.fromTo(
        el,
        { height: oldHeight },
        {
          height: settled,
          duration: SETTLE_S,
          ease: SETTLE_EASE,
          onComplete: () => el.classList.remove('flying'),
        },
      )
    } else {
      setLive(EMPTY_LIVE)
      setPending(sentIn)
      setDraft('')
      setItems((prev) => [...prev, mine])
    }

    try {
      const reply = await api.talk(text, current ?? undefined)
      void loadList(true)
      if (era.current !== sentIn) return
      setItems((prev) => [
        ...prev,
        {
          kind: 'assistant',
          key: nextKey(),
          text: reply.reply,
          reasoning: reply.reasoning,
          thoughtMs: reply.thought_ms,
          steps: reply.steps.map(fromStep),
        },
      ])
      if (current === null) {
        setCurrent(reply.conversation_id)
        wanted.current = reply.conversation_id
      }
    } catch (err) {
      if (era.current !== sentIn) return
      setItems((prev) => [
        ...prev.filter((i) => i.key !== mine.key),
        {
          kind: 'system',
          key: nextKey(),
          text: errorText(err),
          hint: 'Your message is back in the composer.',
        },
      ])
      setDraft(text)
      input.current?.focus()
    } finally {
      inFlight.current = null
      setPending((p) => (p === sentIn ? null : p))
    }
  }

  const onSubmit = (e: FormEvent) => {
    e.preventDefault()
    void send()
  }

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault()
      void send()
    }
  }

  const commitRename = async () => {
    if (!renaming) return
    const { id, value } = renaming
    const title = value.trim()
    setRenaming(null)
    const before = conversations
    if (!title || title === before.find((c) => c.id === id)?.title) return
    setSideNotice(null)
    setConversations((prev) => prev.map((c) => (c.id === id ? { ...c, title } : c)))
    try {
      await api.renameConversation(id, title)
    } catch (err) {
      setConversations(before)
      setSideNotice(errorText(err))
    }
  }

  const remove = (c: Conversation) => {
    // The hold outlives this component, so the commit also pokes the app-level
    // refresh that a remounted view is listening to.
    const settled = () => {
      void loadList(true)
      onChanged()
    }
    deleteHold.start(c.id, () => {
      api.deleteConversation(c.id).then(settled, settled)
    })
    const wasOpen = current === c.id
    if (wasOpen) {
      era.current++
      wanted.current = null
      setCurrent(null)
      setItems([])
      setMsgState('ready')
    }
    setSideNotice(null)
    tick((n) => n + 1)
    notify(`Deleted "${c.title}"`, {
      label: 'Undo',
      windowMs: UNDO_MS,
      run: () => {
        if (!deleteHold.cancel(c.id)) return
        tick((n) => n + 1)
        if (wasOpen) show(c.id)
      },
    })
  }

  const onScroll = () => {
    const el = pane.current
    if (el && !flight.current) stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 64
  }

  const visible = conversations.filter((c) => c.id !== deleteHold.held())

  return (
    <div className="chat">
      {sideOpen && <div className="chat-scrim" onClick={() => setSideOpen(false)} />}
      <aside className={sideOpen ? 'chat-side open' : 'chat-side'}>
        <div className="chat-side-head">
          <button className="chat-new" onClick={startNew}>
            <span aria-hidden="true">+</span> New chat
          </button>
        </div>
        {listState === 'loading' && <p className="chat-side-note muted">Loading chats…</p>}
        {listState === 'error' && (
          <p className="chat-side-note">
            Couldn&rsquo;t load your chats.{' '}
            <button className="chat-link" onClick={() => void loadList()}>
              Try again
            </button>
          </p>
        )}
        {listState === 'ready' && visible.length === 0 && (
          <p className="chat-side-note muted">No chats yet.</p>
        )}
        <ul className="chat-list">
          {visible.map((c) => (
            <li key={c.id} className="chat-row" data-active={c.id === current}>
              {renaming?.id === c.id ? (
                <input
                  className="chat-rename"
                  autoFocus
                  value={renaming.value}
                  aria-label="Chat title"
                  onChange={(e) => setRenaming({ id: c.id, value: e.target.value })}
                  onBlur={() => void commitRename()}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') {
                      e.preventDefault()
                      void commitRename()
                    }
                    if (e.key === 'Escape') setRenaming(null)
                  }}
                />
              ) : (
                <button
                  className="chat-open"
                  aria-current={c.id === current}
                  onClick={() => show(c.id)}
                >
                  {c.title}
                  {c.summary && <span className="chat-gist">{c.summary}</span>}
                </button>
              )}
              <div className="chat-meta">
                <span className="chat-when">
                  {c.via === 'telegram' && (
                    <svg className="chat-via" viewBox="0 0 24 24" role="img">
                      <title>Last answered on Telegram</title>
                      <path d="M21 3L2 11l8 3 3 8z" />
                      <path d="M21 3l-11 11" />
                    </svg>
                  )}
                  {shortDate(c.updated_at)}
                </span>
                <Overflow
                  className="chat-more-wrap"
                  label={`More actions for ${c.title}`}
                  items={[
                    { label: 'Rename', run: () => setRenaming({ id: c.id, value: c.title }) },
                    { label: 'Delete', run: () => remove(c) },
                  ]}
                />
              </div>
            </li>
          ))}
        </ul>
        {sideNotice && <p className="chat-side-note error">{sideNotice}</p>}
      </aside>

      <section className="chat-main">
        {goHome && (
          <div className="chat-head">
            <Pulse session={session} refresh={refresh} onOpen={goHome} />
          </div>
        )}
        <div className="chat-pane" ref={pane} onScroll={onScroll}>
          <div className="chat-stream">
            {msgState === 'loading' && <p className="muted">Loading this chat…</p>}
            {msgState === 'error' && (
              <p className="turn system">
                Couldn&rsquo;t load these messages.{' '}
                <button
                  className="chat-link"
                  onClick={() => current !== null && void loadMessages(current)}
                >
                  Try again
                </button>
              </p>
            )}
            {msgState === 'ready' &&
              grouped(items).map((item) => {
                if (item.kind === 'assistant') return assistantTurn(item)
                if (item.kind === 'steps')
                  return (
                    <Trace key={item.key} label={STEPS_LABEL} reasoning="" steps={item.steps} />
                  )
                return turn(item)
              })}
            {busy && (
              <Trace
                live
                held={flying}
                label={liveLabel(live.steps, Date.now())}
                reasoning={live.reasoning}
                steps={live.steps}
              />
            )}
          </div>
        </div>

        <div className="chat-foot">
          <div className="chat-compose">
            <button
              className="chat-toggle"
              aria-label="Chats"
              aria-expanded={sideOpen}
              onClick={() => setSideOpen((v) => !v)}
            >
              ⋯
            </button>
            <form className={`tellnote${draft.trim() ? ' armed' : ''}`} onSubmit={onSubmit}>
              <textarea
                ref={input}
                rows={1}
                placeholder="Tell Note"
                aria-label="Tell Note"
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={onKeyDown}
              />
              <button type="submit" aria-label="Send" disabled={busy || !draft.trim()}>
                <svg viewBox="0 0 24 24" aria-hidden="true">
                  <path d="M5 12h14" />
                  <path d="M13 6l6 6-6 6" />
                </svg>
              </button>
            </form>
          </div>
        </div>
      </section>
    </div>
  )
}
