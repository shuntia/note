import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
} from 'react'
import { api, ApiError } from '../api'
import { Markdown } from '../markdown'
import type { Conversation, TalkMessage, TalkStep } from '../types'

type Item =
  | { kind: 'user'; key: string; text: string }
  | { kind: 'assistant'; key: string; text: string }
  | { kind: 'tool'; key: string; name: string; args: string; result: string; isError: boolean }
  | { kind: 'system'; key: string; text: string; hint: string | null }

type ToolItem = Extract<Item, { kind: 'tool' }>

type Load = 'loading' | 'ready' | 'error'

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

function ToolBlock({ item }: { item: ToolItem }) {
  const args = pretty(item.args)
  const result = pretty(item.result)
  return (
    <details className={item.isError ? 'tool error' : 'tool'}>
      <summary>
        <span className="tool-glyph" aria-hidden="true" />
        <span className="tool-name">{item.name}</span>
        <span className="tool-badge">{item.isError ? 'error' : 'ok'}</span>
      </summary>
      <div className="tool-body">
        {args && (
          <>
            <div className="tool-label">args</div>
            <pre className="tool-block">{args}</pre>
          </>
        )}
        <div className="tool-label">result</div>
        <pre className="tool-block">{result || '—'}</pre>
      </div>
    </details>
  )
}

export function Talk() {
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
  const [confirming, setConfirming] = useState<number | null>(null)
  const [sideNotice, setSideNotice] = useState<string | null>(null)

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
    setConfirming(null)
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
    setConfirming(null)
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

  const remove = async (id: number) => {
    setConfirming(null)
    setSideNotice(null)
    const before = conversations
    setConversations((prev) => prev.filter((c) => c.id !== id))
    if (current === id) {
      era.current++
      wanted.current = null
      setCurrent(null)
      setItems([])
      setMsgState('ready')
    }
    try {
      await api.deleteConversation(id)
    } catch (err) {
      setConversations(before)
      setSideNotice(errorText(err))
    }
  }

  const onScroll = () => {
    const el = pane.current
    if (el) stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 64
  }

  const active = conversations.find((c) => c.id === current)
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
        {listState === 'ready' && conversations.length === 0 && (
          <p className="chat-side-note muted">No chats yet.</p>
        )}
        <ul className="chat-list">
          {conversations.map((c) => (
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
                {confirming === c.id ? (
                  <>
                    <span className="chat-when">Delete this chat?</span>
                    <span className="chat-tools always">
                      <button className="chat-tool danger" onClick={() => void remove(c.id)}>
                        delete
                      </button>
                      <button className="chat-tool" onClick={() => setConfirming(null)}>
                        keep
                      </button>
                    </span>
                  </>
                ) : (
                  <>
                    <span className="chat-when">{shortDate(c.updated_at)}</span>
                    <span className="chat-tools">
                      <button
                        className="chat-tool"
                        aria-label={`Rename ${c.title}`}
                        onClick={() => setRenaming({ id: c.id, value: c.title })}
                      >
                        rename
                      </button>
                      <button
                        className="chat-tool danger"
                        aria-label={`Delete ${c.title}`}
                        onClick={() => setConfirming(c.id)}
                      >
                        delete
                      </button>
                    </span>
                  </>
                )}
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
              items.map((item) => {
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
                if (item.kind === 'tool') return <ToolBlock key={item.key} item={item} />
                return (
                  <div key={item.key} className="turn system" role="alert">
                    <span>{item.text}</span>
                    {item.hint && <span className="chat-hint">{item.hint}</span>}
                  </div>
                )
              })}
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
        </div>
      </section>
    </div>
  )
}
