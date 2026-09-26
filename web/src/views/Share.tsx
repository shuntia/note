import { useCallback, useEffect, useLayoutEffect, useRef, useState, type FormEvent, type KeyboardEvent } from 'react'
import { api, ApiError } from '../api'
import { Markdown } from '../markdown'
import type { ShareInfo, ShareMessage } from '../types'
import '../styles/talk.css'
import '../styles/share.css'

type Load<T> = T | 'ended' | 'error' | undefined

const DATE = { month: 'long', day: 'numeric' } as const

function coverage(info: ShareInfo): string {
  const s = info.scope
  const parts: string[] = []
  if (s.tasks) parts.push(s.categories.length > 0 ? `${s.categories.join(', ')} tasks` : 'tasks')
  if (s.today) parts.push(s.horizon_days === 1 ? "today's plan" : `the next ${s.horizon_days} days`)
  if (s.goals) parts.push('goals')
  if (s.progress) parts.push('recent progress')
  const list = parts.length <= 1 ? parts.join('') : `${parts.slice(0, -1).join(', ')} and ${parts[parts.length - 1]}`
  const until = new Date(info.expires_at).toLocaleDateString(undefined, DATE)
  return `${info.owner}'s ${list || 'Note'}, shared until ${until}.`
}

export function SharePage({ token }: { token: string }) {
  const [info, setInfo] = useState<Load<ShareInfo>>(undefined)
  const [thread, setThread] = useState<ShareMessage[]>([])
  const [draft, setDraft] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const pane = useRef<HTMLDivElement>(null)
  const input = useRef<HTMLTextAreaElement>(null)
  const sentOnce = useRef(false)
  const threadId = useRef<number | null>(null)
  const stick = useRef(true)

  useEffect(() => {
    api.share
      .info(token)
      .then(setInfo)
      .catch((e: unknown) => setInfo(e instanceof ApiError && e.status === 404 ? 'ended' : 'error'))
  }, [token])

  useEffect(() => {
    const el = pane.current
    if (el && sentOnce.current && stick.current) el.scrollTop = el.scrollHeight - el.clientHeight
  }, [thread, busy, error])

  // Grows the composer with its text up to eight lines, then scrolls inside it.
  const fitComposer = useCallback(() => {
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
  }, [])

  useLayoutEffect(() => {
    fitComposer()
  }, [draft, info, fitComposer])

  const send = async () => {
    const text = draft.trim()
    if (!text || busy) return
    sentOnce.current = true
    stick.current = true
    setBusy(true)
    setError(null)
    setDraft('')
    const asked: ShareMessage = { role: 'user', content: text, created_at: new Date().toISOString() }
    setThread((t) => [...t, asked])
    try {
      const turn = await api.share.send(token, text, threadId.current)
      threadId.current = turn.thread
      const answered: ShareMessage = { role: 'assistant', content: turn.reply, created_at: new Date().toISOString() }
      const stored = turn.note ? await api.share.messages(token, turn.thread).catch(() => null) : null
      setThread((t) => stored ?? [...t, answered])
    } catch (err) {
      setThread((t) => t.filter((m) => m !== asked))
      setDraft(text)
      if (err instanceof ApiError && err.status === 404) setInfo('ended')
      else if (err instanceof ApiError && err.status === 429)
        setError(
          err.message.includes('limit')
            ? 'This link has reached today’s limit; try again tomorrow.'
            : 'Too many messages from this address; try again in a little while.',
        )
      else setError('Note could not answer. Try again.')
    } finally {
      setBusy(false)
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

  const onScroll = () => {
    const el = pane.current
    if (el) stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 64
  }

  if (info === 'ended') {
    return (
      <main className="share-state">
        <h1>This link has ended</h1>
        <p>Ask the person who shared it for a new one.</p>
      </main>
    )
  }
  if (info === undefined) return null
  if (info === 'error') {
    return (
      <main className="share-state">
        <h1>Note is not reachable</h1>
        <p>Try again in a moment.</p>
      </main>
    )
  }

  return (
    <div className="chat share-chat">
      <section className="chat-main">
        <div className="chat-head">
          <h1 className="chat-title">
            <span className="chat-retitle plain">{info.owner}</span>
          </h1>
        </div>
        <p className="share-cover">{coverage(info)}</p>
        <div className="chat-pane" ref={pane} onScroll={onScroll}>
          <div className="chat-stream" role="log" aria-live="polite">
            {thread.length === 0 && (
              <p className="chat-empty">Ask what {info.owner} has today, what is done, or what is due.</p>
            )}
            {thread.map((m, i) => {
              if (m.role === 'note')
                return (
                  <div key={i} className="turn system">
                    Sent to {info.owner}: {m.content}
                  </div>
                )
              if (m.role === 'assistant')
                return (
                  <div key={i} className="reply">
                    <div className="turn assistant">
                      <Markdown text={m.content} />
                    </div>
                  </div>
                )
              return (
                <div key={i} className="turn user">
                  {m.content}
                </div>
              )
            })}
            {busy && <div className="turn pending">Note is thinking</div>}
            {error && (
              <div className="turn system" role="alert">
                {error}
              </div>
            )}
          </div>
        </div>

        <div className="chat-foot">
          <div className="chat-compose">
            <form className={`tellnote${draft.trim() ? ' armed' : ''}`} onSubmit={onSubmit}>
              <textarea
                ref={input}
                rows={1}
                placeholder={info.notes ? `Ask, or leave a note for ${info.owner}` : `Ask about ${info.owner}’s day`}
                aria-label="Ask Note"
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
