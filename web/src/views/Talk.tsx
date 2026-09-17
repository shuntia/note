import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
  type ReactNode,
} from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { Markdown } from '../markdown'
import { Overflow } from '../overflow'
import { doing, receipt } from '../receipts'
import { makeHold } from '../held'
import { forgetConversation, lastConversation, rememberConversation } from '../tellnote'
import { onAgentFrame, type AgentFrame } from '../ws'
import type { Conversation, TalkMessage, TalkStep } from '../types'

type Item =
  | { kind: 'user'; key: string; text: string }
  | { kind: 'assistant'; key: string; text: string }
  | { kind: 'tool'; key: string; name: string; args: string; result: string; isError: boolean }
  | { kind: 'activity'; key: string; reasoning: string; steps: ToolItem[] }
  | { kind: 'system'; key: string; text: string; hint: string | null }

type ToolItem = Extract<Item, { kind: 'tool' }> & { running?: boolean }
type ActivityItem = Extract<Item, { kind: 'activity' }>
type TurnItem = Extract<Item, { kind: 'user' | 'assistant' | 'system' }>

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
  steps[ev.index] =
    ev.kind === 'tool_call'
      ? { kind: 'tool', key: `live-${ev.index}`, name: ev.name, args: ev.args, result: '', isError: false, running: true }
      : {
          kind: 'tool',
          key: `live-${ev.index}`,
          name: ev.name,
          args: before?.args ?? '',
          result: ev.result,
          isError: ev.is_error,
          running: false,
        }
  return { ...at, steps }
}

type Load = 'loading' | 'ready' | 'error'

const UNDO_MS = 10_000

// Deleting a conversation has no server-side reversal, so the request waits out the
// undo window before it is sent.
const deleteHold = makeHold<number>(UNDO_MS)

let sequence = 0
const nextKey = () => `local-${++sequence}`

function fromMessage(m: TalkMessage): Item {
  const key = `msg-${m.id}`
  if (m.role === 'user') return { kind: 'user', key, text: m.content }
  if (m.role === 'assistant') return { kind: 'assistant', key, text: m.content }
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

// What the header says a block is doing, or what it did once it is over.
function status(steps: ToolItem[], reasoning: string, live: boolean): string {
  const busy = steps.find((s) => s.running)
  if (live) return busy ? `${doing(busy.name, busy.args)}…` : 'Thinking…'
  const thought = reasoning ? 'Thought' : ''
  if (steps.length === 0) return thought || 'No steps'
  const count = `${steps.length} step${steps.length === 1 ? '' : 's'}`
  return thought ? `${thought} · ${count}` : count
}

// One line while it runs and after: the reasoning and the calls stay a chevron away.
function ActivityBlock({
  steps,
  reasoning,
  live = false,
}: {
  steps: ToolItem[]
  reasoning: string
  live?: boolean
}) {
  const [open, setOpen] = useState(false)
  const state = [live ? 'live' : '', open ? 'open' : ''].filter(Boolean).join(' ')
  return (
    <div className={`activity ${state}`.trim()}>
      <button className="activity-head" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
        <span className="activity-status" aria-live="polite">
          {status(steps, reasoning, live)}
        </span>
        <svg className="receipt-chev" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M9 6l6 6-6 6" />
        </svg>
      </button>
      {open && (
        <div className="activity-body">
          {reasoning && <pre className="activity-think">{reasoning}</pre>}
          <div className="receipts">
            {steps.map((step) => (
              <Receipt key={step.key} item={step} />
            ))}
          </div>
        </div>
      )}
    </div>
  )
}

// Consecutive calls read as one activity block, and it sits under the reply that
// explains it even though the transcript records the calls first.
function grouped(items: Item[]): (ActivityItem | TurnItem)[] {
  const out: (ActivityItem | TurnItem)[] = []
  for (const item of items) {
    const last = out[out.length - 1]
    if (item.kind !== 'tool') out.push(item)
    else if (last?.kind === 'activity') last.steps.push(item)
    else out.push({ kind: 'activity', key: item.key, reasoning: '', steps: [item] })
  }
  for (let i = 0; i < out.length - 1; i++) {
    if (out[i].kind === 'activity' && out[i + 1].kind === 'assistant') {
      ;[out[i], out[i + 1]] = [out[i + 1], out[i]]
      i++
    }
  }
  return out
}

function turn(item: TurnItem): ReactNode {
  if (item.kind === 'user')
    return (
      <div key={item.key} className="turn user">
        {item.text}
      </div>
    )
  if (item.kind === 'assistant')
    return (
      <div key={item.key} className="turn assistant">
        <Markdown text={item.text} />
      </div>
    )
  return (
    <div key={item.key} className="turn system" role="alert">
      <span>{item.text}</span>
      {item.hint && <span className="chat-hint">{item.hint}</span>}
    </div>
  )
}

export function Talk({
  notify,
  onChanged,
  prefill,
  onPrefilled,
}: ViewProps & { prefill?: string | null; onPrefilled?: () => void }) {
  const [conversations, setConversations] = useState<Conversation[]>([])
  const [listState, setListState] = useState<Load>('loading')
  const [current, setCurrent] = useState<number | null>(null)
  const [items, setItems] = useState<Item[]>([])
  const [msgState, setMsgState] = useState<Load>('ready')
  const [draft, setDraft] = useState(prefill ?? '')
  // era of the send in flight, so its pending row belongs to the conversation that sent it
  const [pending, setPending] = useState<number | null>(null)
  const [live, setLive] = useState<Live>(EMPTY_LIVE)
  const [sideOpen, setSideOpen] = useState(false)
  const [renaming, setRenaming] = useState<{ id: number; value: string } | null>(null)
  const [sideNotice, setSideNotice] = useState<string | null>(null)
  const [, tick] = useState(0)

  const pane = useRef<HTMLDivElement>(null)
  const input = useRef<HTMLTextAreaElement>(null)
  const stick = useRef(true)
  const wanted = useRef<number | null>(null)
  // bumped whenever the open conversation changes, so a late reply never lands in the wrong pane
  const era = useRef(0)
  const busy = pending === era.current
  // the send in flight, for the frames arriving on the shell's socket
  const inFlight = useRef<{ conversation: number | null } | null>(null)

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

  useEffect(() => {
    const el = pane.current
    if (el && stick.current) el.scrollTop = el.scrollHeight
  }, [items, busy, live, msgState])

  useEffect(() => {
    const el = input.current
    if (!el) return
    el.style.height = 'auto'
    const style = window.getComputedStyle(el)
    const line = parseFloat(style.lineHeight) || 22
    const frame =
      parseFloat(style.paddingTop) +
      parseFloat(style.paddingBottom) +
      parseFloat(style.borderTopWidth) +
      parseFloat(style.borderBottomWidth)
    const max = line * 8 + frame
    el.style.height = `${Math.min(el.scrollHeight, max)}px`
    el.style.overflowY = el.scrollHeight > max ? 'auto' : 'hidden'
  }, [draft])

  // Talk mounts fresh on every view switch, so this arrives on a new conversation.
  useEffect(() => {
    if (!prefill) return
    setDraft(prefill)
    const el = input.current
    if (el) {
      el.focus()
      el.setSelectionRange(el.value.length, el.value.length)
    }
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

  const open = (id: number) => {
    setSideOpen(false)
    setSideNotice(null)
    if (id === current) return
    stick.current = true
    era.current++
    setItems([])
    setCurrent(id)
    rememberConversation(id)
    void loadMessages(id)
  }

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

  const send = async () => {
    const text = draft.trim()
    // a send while history is still loading would be wiped by the load's setItems
    if (!text || busy || msgState === 'loading') return
    const mine: Item = { kind: 'user', key: nextKey(), text }
    const sentIn = era.current
    stick.current = true
    inFlight.current = { conversation: current }
    setLive(EMPTY_LIVE)
    setPending(sentIn)
    setDraft('')
    setItems((prev) => [...prev, mine])
    try {
      const reply = await api.talk(text, current ?? undefined)
      void loadList(true)
      if (era.current !== sentIn) return
      const activity: Item[] =
        reply.steps.length || reply.reasoning
          ? [
              {
                kind: 'activity',
                key: nextKey(),
                reasoning: reply.reasoning,
                steps: reply.steps.map(fromStep),
              },
            ]
          : []
      setItems((prev) => [
        ...prev,
        ...activity,
        { kind: 'assistant', key: nextKey(), text: reply.reply },
      ])
      rememberConversation(reply.conversation_id)
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
    const wasRemembered = lastConversation() === c.id
    if (wasRemembered) forgetConversation()
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
        if (wasOpen) open(c.id)
        else if (wasRemembered) rememberConversation(c.id)
      },
    })
  }

  // A fresh browser still opens on something: the remembered thread if it is still
  // there, otherwise the one touched last.
  const opened = useRef(false)
  useEffect(() => {
    if (opened.current || listState !== 'ready' || current !== null) return
    opened.current = true
    if (prefill) return
    const pool = conversations.filter((c) => c.id !== deleteHold.held())
    const remembered = pool.find((c) => c.id === lastConversation())
    const recent = remembered ?? [...pool].sort((a, b) => b.updated_at.localeCompare(a.updated_at))[0]
    if (recent) open(recent.id)
  })

  const onScroll = () => {
    const el = pane.current
    if (el) stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 64
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
                  onClick={() => open(c.id)}
                >
                  {c.title}
                </button>
              )}
              <div className="chat-meta">
                <span className="chat-when">{shortDate(c.updated_at)}</span>
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
              grouped(items).map((item) =>
                item.kind === 'activity' ? (
                  <ActivityBlock key={item.key} steps={item.steps} reasoning={item.reasoning} />
                ) : (
                  turn(item)
                ),
              )}
            {busy && <ActivityBlock live steps={live.steps} reasoning={live.reasoning} />}
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
            <form className="tellnote" onSubmit={onSubmit}>
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
