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
import { receipt } from '../receipts'
import type { Conversation, TalkMessage, TalkStep } from '../types'

type Item =
  | { kind: 'user'; key: string; text: string }
  | { kind: 'assistant'; key: string; text: string }
  | { kind: 'tool'; key: string; name: string; args: string; result: string; isError: boolean }
  | { kind: 'system'; key: string; text: string; hint: string | null }

type ToolItem = Extract<Item, { kind: 'tool' }>

type Load = 'loading' | 'ready' | 'error'

const UNDO_MS = 10_000

// Deleting a conversation has no server-side reversal, so the request waits out the
// undo window before it is sent. Module scope keeps the hold alive across remounts.
let heldDelete: { id: number; timer: number } | null = null

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

const fromStep = (s: TalkStep): Item => ({
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
        <span className="receipt-mark" aria-hidden="true">
          {item.isError ? '✕' : '✓'}
        </span>
        <span className="receipt-text">{receipt(item.name, item.args, item.isError)}</span>
        <span className="receipt-chev" aria-hidden="true">
          {open ? '▾' : '▸'}
        </span>
      </button>
      {open && (
        <div className="receipt-body">
          <div className="receipt-tool">{item.name}</div>
          {args && <pre className="receipt-block">{args}</pre>}
          <pre className="receipt-block">{result || '—'}</pre>
        </div>
      )}
    </div>
  )
}

// Consecutive calls read as one receipt block, and it sits under the reply that
// explains it even though the transcript records the calls first.
function grouped(items: Item[]): { key: string; items: Item[] }[] {
  const out: { key: string; items: Item[] }[] = []
  for (const item of items) {
    const last = out[out.length - 1]
    if (item.kind === 'tool' && last?.items[0].kind === 'tool') last.items.push(item)
    else out.push({ key: item.key, items: [item] })
  }
  for (let i = 0; i < out.length - 1; i++) {
    if (out[i].items[0].kind === 'tool' && out[i + 1].items[0].kind === 'assistant') {
      ;[out[i], out[i + 1]] = [out[i + 1], out[i]]
      i++
    }
  }
  return out
}

function turn(item: Exclude<Item, ToolItem>): ReactNode {
  if (item.kind === 'user')
    return (
      <div key={item.key} className="turn user">
        {item.text}
      </div>
    )
  if (item.kind === 'assistant')
    return (
      <div key={item.key} className="turn assistant">
        <span className="turn-avatar" aria-hidden="true" />
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

export function Talk({ notify }: ViewProps) {
  const [conversations, setConversations] = useState<Conversation[]>([])
  const [listState, setListState] = useState<Load>('loading')
  const [current, setCurrent] = useState<number | null>(null)
  const [items, setItems] = useState<Item[]>([])
  const [msgState, setMsgState] = useState<Load>('ready')
  const [draft, setDraft] = useState('')
  // era of the send in flight, so its pending row belongs to the conversation that sent it
  const [pending, setPending] = useState<number | null>(null)
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

  useEffect(() => {
    const el = pane.current
    if (el && stick.current) el.scrollTop = el.scrollHeight
  }, [items, busy, msgState])

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
    setPending(sentIn)
    setDraft('')
    setItems((prev) => [...prev, mine])
    try {
      const reply = await api.talk(text, current ?? undefined)
      void loadList(true)
      if (era.current !== sentIn) return
      setItems((prev) => [
        ...prev,
        ...reply.steps.map(fromStep),
        { kind: 'assistant', key: nextKey(), text: reply.reply },
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

  const commitDelete = useCallback(() => {
    if (!heldDelete) return
    const { id, timer } = heldDelete
    heldDelete = null
    window.clearTimeout(timer)
    api.deleteConversation(id).then(
      () => void loadList(true),
      () => void loadList(true),
    )
  }, [loadList])

  useEffect(() => commitDelete, [commitDelete])

  const remove = (c: Conversation) => {
    commitDelete()
    heldDelete = { id: c.id, timer: window.setTimeout(commitDelete, UNDO_MS) }
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
        if (heldDelete?.id !== c.id) return
        window.clearTimeout(heldDelete.timer)
        heldDelete = null
        tick((n) => n + 1)
        if (wasOpen) open(c.id)
      },
    })
  }

  const onScroll = () => {
    const el = pane.current
    if (el) stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 64
  }

  const visible = conversations.filter((c) => c.id !== heldDelete?.id)
  const active = visible.find((c) => c.id === current)
  const title = current === null ? 'New chat' : (active?.title ?? 'Chat')

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
        <header className="chat-head">
          <button
            className="chat-toggle"
            aria-expanded={sideOpen}
            onClick={() => setSideOpen((v) => !v)}
          >
            Chats
          </button>
          <h2>{title}</h2>
        </header>

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
            {msgState === 'ready' && items.length === 0 && !busy && (
              <div className="chat-empty">
                <span className="chat-empty-glyph" aria-hidden="true" />
                <p>Ask Note anything — about today, your tasks, or what to do next.</p>
              </div>
            )}
            {msgState === 'ready' &&
              grouped(items).map((group) =>
                group.items[0].kind === 'tool' ? (
                  <div key={group.key} className="receipts">
                    {group.items.map((item) => (
                      <Receipt key={item.key} item={item as ToolItem} />
                    ))}
                  </div>
                ) : (
                  turn(group.items[0] as Exclude<Item, ToolItem>)
                ),
              )}
            {busy && (
              <p className="turn pending" aria-live="polite">
                Note is thinking…
              </p>
            )}
          </div>
        </div>

        <div className="chat-foot">
          <form className="chat-composer" onSubmit={onSubmit}>
            <textarea
              ref={input}
              className="chat-input"
              rows={1}
              placeholder="Message Note…"
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={onKeyDown}
            />
            <button
              className="primary chat-send"
              aria-label="Send message"
              disabled={busy || !draft.trim()}
            >
              <svg viewBox="0 0 20 20" fill="none" aria-hidden="true">
                <path
                  d="M10 16V4M10 4L4.5 9.5M10 4l5.5 5.5"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </svg>
            </button>
          </form>
          <p className="chat-standing">
            Every change Note makes shows up above — nothing happens silently.
          </p>
        </div>
      </section>
    </div>
  )
}
