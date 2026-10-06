import { useCallback, useEffect, useLayoutEffect, useRef, useState, type FormEvent, type KeyboardEvent } from 'react'
import { api, ApiError } from '../api'
import { t } from '../i18n'
import * as format from '../i18n/format'
import { Markdown } from '../markdown'
import type { ShareInfo, ShareMessage } from '../types'
import '../styles/talk.css'
import '../styles/share.css'

type Load<T> = T | 'ended' | 'error' | undefined

function coverage(info: ShareInfo): string {
  const s = info.scope
  const parts: string[] = []
  if (s.tasks)
    parts.push(
      s.categories.length > 0
        ? t('share.scope.categoryTasks', { categories: format.list(s.categories) })
        : t('share.scope.tasks'),
    )
  if (s.today)
    parts.push(s.horizon_days === 1 ? t('share.scope.today') : t('share.scope.days', { count: s.horizon_days }))
  if (s.goals) parts.push(t('share.scope.goals'))
  if (s.progress) parts.push(t('share.scope.progress'))
  const until = format.monthDay(new Date(info.expires_at))
  return parts.length > 0
    ? t('share.cover', { owner: info.owner, list: format.list(parts), until })
    : t('share.coverAll', { owner: info.owner, until })
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
          err.message.includes('limit') ? t('share.dailyLimit') : t('share.rateLimited'),
        )
      else setError(t('share.failed'))
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
        <h1>{t('share.ended')}</h1>
        <p>{t('share.endedHint')}</p>
      </main>
    )
  }
  if (info === undefined) return null
  if (info === 'error') {
    return (
      <main className="share-state">
        <h1>{t('share.unreachable')}</h1>
        <p>{t('share.unreachableHint')}</p>
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
              <p className="chat-empty">{t('share.empty', { owner: info.owner })}</p>
            )}
            {thread.map((m, i) => {
              if (m.role === 'note')
                return (
                  <div key={i} className="turn system">
                    {t('share.sentTo', { owner: info.owner, text: m.content })}
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
            {busy && <div className="turn pending">{t('share.thinking')}</div>}
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
                placeholder={
                  info.notes
                    ? t('share.askOrLeaveNote', { owner: info.owner })
                    : t('share.askAboutDay', { owner: info.owner })
                }
                aria-label={t('share.ask')}
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={onKeyDown}
              />
              <button type="submit" aria-label={t('share.send')} disabled={busy || !draft.trim()}>
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
