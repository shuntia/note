import {
  Fragment,
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
import { ComposeButton } from '../call/ComposeButton'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { t, type Key } from '../i18n'
import { day } from '../i18n/format'
import { Markdown } from '../markdown'
import { reducedMotion } from '../motion'
import { Overflow } from '../overflow'
import { Pulse } from '../pulse'
import { batchReceipts, doing, receipt } from '../receipts'
import { makeHold } from '../held'
import type { FocusSession } from '../session'
import { onAgentFrame, type AgentFrame } from '../ws'
import type { Conversation, FailureReason, TalkMessage, TalkStep } from '../types'
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
  | {
      kind: 'tool'
      key: string
      name: string
      args: string
      result: string
      isError: boolean
      thinking?: string
    }
  | { kind: 'steps'; key: string; steps: ToolItem[] }
  | { kind: 'system'; key: string; text: string; hint: string | null; detail?: string }

type ToolItem = Extract<Item, { kind: 'tool' }> & { running?: boolean; finishedAt?: number }
type AssistantItem = Extract<Item, { kind: 'assistant' }>
type StepsItem = Extract<Item, { kind: 'steps' }>
type TurnItem = Extract<Item, { kind: 'user' | 'system' }>
type Shown = AssistantItem | StepsItem | TurnItem

// One session's progress as the live frames describe it; `seq` is the last one
// applied and `reasoning` the thinking no call has followed yet.
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
  const opening = call && !before
  steps[ev.index] = {
    kind: 'tool',
    key: `live-${ev.index}`,
    name: ev.name,
    args: call ? ev.args : (before?.args ?? ''),
    result: call ? '' : ev.result,
    isError: call ? false : ev.is_error,
    thinking: opening ? at.reasoning || undefined : before?.thinking,
    running: call,
    finishedAt: call ? undefined : Date.now(),
  }
  return { ...at, reasoning: opening ? '' : at.reasoning, steps }
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
    thinking: m.reasoning ?? undefined,
  }
}

const fromStep = (s: TalkStep): ToolItem => ({
  kind: 'tool',
  key: nextKey(),
  name: s.name,
  args: s.args,
  result: s.result,
  isError: s.is_error,
  thinking: s.thinking ?? undefined,
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

const FAILURE_KEYS: Record<FailureReason, Key> = {
  model_unavailable: 'failure.modelUnavailable',
  auth_invalid: 'failure.authInvalid',
  out_of_credits: 'failure.outOfCredits',
  rate_limited: 'failure.rateLimited',
  provider_down: 'failure.providerDown',
  context_too_long: 'failure.contextTooLong',
  refused: 'failure.refused',
  internal: 'failure.internal',
}

function errorText(err: unknown): string {
  if (!(err instanceof ApiError)) return t('talk.unreachable')
  const key = err.reason && FAILURE_KEYS[err.reason]
  return key ? t(key) : err.message
}

function shortDate(iso: string): string {
  const at = new Date(iso)
  if (Number.isNaN(at.getTime())) return ''
  return day(at)
}

const Mark = ({ isError, running }: { isError: boolean; running?: boolean }) => (
  <svg
    className={running ? 'receipt-mark running' : 'receipt-mark'}
    viewBox="0 0 24 24"
    aria-hidden="true"
  >
    {running && <path d="M12 3a9 9 0 0 1 9 9" />}
    {!running && (isError ? <path d="M6 6l12 12M18 6L6 18" /> : <path d="M5 12.5l4.5 4.5L19 7.5" />)}
  </svg>
)

function Receipt({ item }: { item: ToolItem }) {
  const [open, setOpen] = useState(false)
  const args = pretty(item.args)
  const result = pretty(item.result)
  const inner = item.name === 'batch' ? batchReceipts(item.args, item.result) : []
  const state = [item.isError ? 'error' : '', open ? 'open' : ''].filter(Boolean).join(' ')
  return (
    <div className={`receipt ${state}`.trim()}>
      <button className="receipt-chip" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
        <Mark isError={item.isError} running={item.running} />
        <span className="receipt-text">
          {item.running ? doing(item.name, item.args) : receipt(item.name, item.args, item.isError)}
        </span>
        <svg className="receipt-chev" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M9 6l6 6-6 6" />
        </svg>
      </button>
      {inner.length > 0 && (
        <div className="receipt-inner">
          {inner.map((one, i) => (
            <div key={i} className={one.isError ? 'receipt-sub error' : 'receipt-sub'}>
              <Mark isError={one.isError} />
              <span className="receipt-text">{receipt(one.name, one.args, one.isError)}</span>
            </div>
          ))}
        </div>
      )}
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

// The header over a finished reply; null when it would open onto nothing.
function thoughtLabel(item: AssistantItem): string | null {
  if (!item.reasoning && item.steps.length === 0) return null
  if (item.thoughtMs === null) return t('talk.steps')
  if (item.thoughtMs < 1000) return t('talk.thoughtMoment')
  return t('talk.thoughtFor', { count: Math.round(item.thoughtMs / 1000) })
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
  if (running) return t('talk.thinkingWith', { step: doing(running.name, running.args) })
  const just = lastFinished(steps)
  if (just?.finishedAt && now - just.finishedAt < RECEIPT_MS)
    return t('talk.thinkingWith', { step: receipt(just.name, just.args, just.isError) })
  return t('talk.thinking')
}

// One quiet line; the reasoning and the calls stay a chevron away, each round's
// thinking above the calls it led to and `reasoning` — the round that answered —
// last. `held` keeps a live line in the layout but out of sight until the sent
// bubble has landed.
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
          <div className="receipts">
            {steps.map((step) => (
              <Fragment key={step.key}>
                {step.thinking && <pre className="activity-think">{step.thinking}</pre>}
                <Receipt item={step} />
              </Fragment>
            ))}
            {reasoning && <pre className="activity-think">{reasoning}</pre>}
          </div>
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

// A title being edited, and which of the two places is editing it.
type Rename = { id: number; value: string; at: 'list' | 'head' }

const COLUMN = '(min-width: 1088px)'
const LIST_KEY = 'note.chatListOpen'

function readListOpen(): boolean {
  try {
    return window.localStorage.getItem(LIST_KEY) !== 'closed'
  } catch {
    return true
  }
}

function writeListOpen(open: boolean) {
  try {
    window.localStorage.setItem(LIST_KEY, open ? 'open' : 'closed')
  } catch {
    return
  }
}

/** True where the thread list is a column in the flow rather than a drawer. */
function useColumn(): boolean {
  const [column, setColumn] = useState(() => window.matchMedia(COLUMN).matches)
  useEffect(() => {
    const mq = window.matchMedia(COLUMN)
    const read = () => setColumn(mq.matches)
    read()
    mq.addEventListener('change', read)
    return () => mq.removeEventListener('change', read)
  }, [])
  return column
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
      {item.detail ? (
        <details className="turn-why">
          <summary>{item.text}</summary>
          <code>{item.detail}</code>
        </details>
      ) : (
        <span>{item.text}</span>
      )}
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
  onCall,
  micBlocked = false,
}: ViewProps & {
  prefill?: string | null
  onPrefilled?: () => void
  // A thread to land on, from a check-in or a deep link; `at` tells one ask from the next.
  open?: { id: number; at: number } | null
  onOpened?: () => void
  session?: FocusSession | null
  goHome?: () => void
  onCall?: (conversationId: number | null) => void
  micBlocked?: boolean
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
  const column = useColumn()
  const [listOpen, setListOpen] = useState(readListOpen)
  const [renaming, setRenaming] = useState<Rename | null>(null)
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
  const listed = useRef(false)
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
    void loadList(listed.current)
    listed.current = true
  }, [refresh, loadList])

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
          hint: t('talk.backInComposer'),
          detail: err instanceof ApiError ? err.detail : undefined,
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
    notify(t('talk.deleted', { title: c.title }), {
      label: t('toast.undo'),
      windowMs: UNDO_MS,
      run: () => {
        if (!deleteHold.cancel(c.id)) return
        tick((n) => n + 1)
        if (wasOpen) show(c.id)
      },
    })
  }

  const toggleList = () => {
    if (!column) {
      setSideOpen((v) => !v)
      return
    }
    const next = !listOpen
    setListOpen(next)
    writeListOpen(next)
  }

  const onScroll = () => {
    const el = pane.current
    if (el && !flight.current) stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 64
  }

  const visible = conversations.filter((c) => c.id !== deleteHold.held())
  const here = current === null ? null : (conversations.find((c) => c.id === current) ?? null)
  const shown = column ? listOpen : sideOpen
  const renameInput = (at: 'list' | 'head', id: number) => (
    <input
      className="chat-rename"
      autoFocus
      value={renaming?.value ?? ''}
      aria-label={t('talk.chatTitle')}
      onChange={(e) => setRenaming({ id, value: e.target.value, at })}
      onBlur={() => void commitRename()}
      onKeyDown={(e) => {
        if (e.key === 'Enter') {
          e.preventDefault()
          void commitRename()
        }
        if (e.key === 'Escape') setRenaming(null)
      }}
    />
  )

  return (
    <div className={`chat${column && listOpen ? ' with-column' : ''}`}>
      {!column && sideOpen && <div className="chat-scrim" onClick={() => setSideOpen(false)} />}
      {(!column || listOpen) && (
        <aside className={`chat-side${sideOpen ? ' open' : ''}`} aria-label={t('talk.chats')}>
          {!column && (
            <div className="chat-side-head">
              <button className="chat-new" onClick={startNew}>
                <span aria-hidden="true">+</span> {t('talk.newChat')}
              </button>
            </div>
          )}
          {listState === 'loading' && <p className="chat-side-note muted">{t('talk.loadingChats')}</p>}
          {listState === 'error' && (
            <p className="chat-side-note">
              {t('talk.listFailed')}{' '}
              <button className="chat-link" onClick={() => void loadList()}>
                {t('talk.tryAgain')}
              </button>
            </p>
          )}
          {listState === 'ready' && visible.length === 0 && (
            <p className="chat-side-note muted">{t('talk.noChats')}</p>
          )}
          <ul className="chat-list">
            {visible.map((c) => (
              <li key={c.id} className="chat-row" data-active={c.id === current}>
                {renaming?.id === c.id && renaming.at === 'list' ? (
                  renameInput('list', c.id)
                ) : (
                  <button
                    className="chat-open"
                    aria-current={c.id === current}
                    onClick={() => show(c.id)}
                  >
                    <span className={c.title_kind === 'draft' ? 'chat-name draft' : 'chat-name'}>
                      {c.title}
                    </span>
                    {c.summary && <span className="chat-gist">{c.summary}</span>}
                  </button>
                )}
                <div className="chat-meta">
                  <span className="chat-when">
                    {c.via === 'matrix' && (
                      <svg className="chat-via" viewBox="0 0 24 24" role="img">
                        <title>{t('talk.viaMatrix')}</title>
                        <path d="M5 4H3v16h2" />
                        <path d="M19 4h2v16h-2" />
                      </svg>
                    )}
                    {shortDate(c.updated_at)}
                  </span>
                  <Overflow
                    className="chat-more-wrap"
                    row=".chat-row"
                    label={t('talk.moreFor', { title: c.title })}
                    items={[
                      {
                        label: t('talk.rename'),
                        run: () => setRenaming({ id: c.id, value: c.title, at: 'list' }),
                      },
                      { label: t('talk.delete'), kind: 'danger', run: () => remove(c) },
                    ]}
                  />
                </div>
              </li>
            ))}
          </ul>
          {sideNotice && <p className="chat-side-note error">{sideNotice}</p>}
        </aside>
      )}

      <section className="chat-main">
        <div className="chat-head">
          <button
            className="chat-chats"
            aria-label={t('talk.chats')}
            aria-expanded={shown}
            onClick={toggleList}
          >
            <svg className="chat-chats-glyph" viewBox="0 0 24 24" aria-hidden="true">
              <path d="M4 7h16" />
              <path d="M4 12h16" />
              <path d="M4 17h16" />
            </svg>
            <span className="chat-chats-label">{t('talk.chats')}</span>
          </button>
          <h1 className="chat-title">
            {here && renaming?.id === here.id && renaming.at === 'head' ? (
              renameInput('head', here.id)
            ) : here ? (
              <button
                className="chat-retitle"
                title={t('talk.renameTitle')}
                onClick={() => setRenaming({ id: here.id, value: here.title, at: 'head' })}
              >
                {here.title}
              </button>
            ) : (
              <span className="chat-retitle plain">{t('talk.newChat')}</span>
            )}
          </h1>
          <button className="btn-round chat-add" aria-label={t('talk.newChat')} onClick={startNew}>
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M12 5v14" />
              <path d="M5 12h14" />
            </svg>
          </button>
          {goHome && <Pulse session={session} refresh={refresh} onOpen={goHome} />}
        </div>
        <div className="chat-pane" ref={pane} onScroll={onScroll}>
          <div className="chat-stream">
            {msgState === 'loading' && <p className="muted">{t('talk.loadingChat')}</p>}
            {msgState === 'ready' && current === null && items.length === 0 && (
              <p className="chat-empty">{t('talk.empty')}</p>
            )}
            {msgState === 'error' && (
              <p className="turn system">
                {t('talk.messagesFailed')}{' '}
                <button
                  className="chat-link"
                  onClick={() => current !== null && void loadMessages(current)}
                >
                  {t('talk.tryAgain')}
                </button>
              </p>
            )}
            {msgState === 'ready' &&
              grouped(items).map((item) => {
                if (item.kind === 'assistant') return assistantTurn(item)
                if (item.kind === 'steps')
                  return (
                    <Trace key={item.key} label={t('talk.steps')} reasoning="" steps={item.steps} />
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
            <form className={`tellnote${draft.trim() ? ' armed' : ''}`} onSubmit={onSubmit}>
              <textarea
                ref={input}
                rows={1}
                placeholder={t('talk.tellNote')}
                aria-label={t('talk.tellNote')}
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={onKeyDown}
              />
              <ComposeButton
                draft={draft}
                busy={busy}
                micBlocked={micBlocked}
                onCall={onCall && (() => onCall(current))}
              />
            </form>
          </div>
        </div>
      </section>
    </div>
  )
}
