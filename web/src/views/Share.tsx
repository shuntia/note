import { useEffect, useLayoutEffect, useRef, useState, type FormEvent } from 'react'
import { api, ApiError } from '../api'
import { Markdown } from '../markdown'
import type { ShareInfo, ShareMessage, ShareTask, ShareView } from '../types'
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

function dueLabel(iso: string | null): string | null {
  if (iso === null) return null
  const due = new Date(iso)
  if (Number.isNaN(due.getTime())) return null
  if (due.getTime() < Date.now()) return 'overdue'
  return `due ${due.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })}`
}

function TaskRow({ task }: { task: ShareTask }) {
  const due = dueLabel(task.due_at)
  const urgent = task.urgency === 'high' || (task.pressing && due !== 'overdue')
  return (
    <div className="share-row">
      <div className="share-row-main">
        <span className="share-title">{task.title}</span>
        <span className="share-meta">
          {urgent && <span className="meta sun">urgent</span>}
          {task.steps > 0 && <span className="meta">{task.done_steps} of {task.steps} steps done</span>}
          {due && <span className={`meta${due === 'overdue' ? ' warn' : ''}`}>{due}</span>}
          {task.goal_title && <span className="meta">for {task.goal_title}</span>}
        </span>
      </div>
      {task.description && <p className="share-desc">{task.description}</p>}
    </div>
  )
}

function Panels({ view }: { view: ShareView }) {
  return (
    <>
      {view.days && (
        <section className="share-panel">
          <h2>Plan</h2>
          {view.days.map((d, i) => (
            <div key={d.date} className="share-day">
              <h3>{i === 0 ? 'Today' : new Date(`${d.date}T00:00:00`).toLocaleDateString(undefined, { weekday: 'long', month: 'short', day: 'numeric' })}</h3>
              {d.rows.length === 0 && <p className="share-empty">Nothing on the plan.</p>}
              {d.rows.map((r, j) => (
                <div key={j} className={`share-row${r.busy ? ' busy' : ''}${r.status === 'done' ? ' done' : ''}`}>
                  <span className="share-when">{r.start}{r.end ? `–${r.end}` : ''}</span>
                  <span className="share-title">{r.title}</span>
                </div>
              ))}
            </div>
          ))}
        </section>
      )}
      {view.tasks && (
        <section className="share-panel">
          <h2>Open tasks <span className="share-count">{view.tasks.length}</span></h2>
          {view.tasks.length === 0 && <p className="share-empty">Nothing open.</p>}
          {view.tasks.map((t) => <TaskRow key={t.id} task={t} />)}
        </section>
      )}
      {view.goals && (
        <section className="share-panel">
          <h2>Goals</h2>
          {view.goals.length === 0 && <p className="share-empty">No goals shared.</p>}
          {view.goals.map((g) => (
            <div key={g.id} className="share-row">
              <div className="share-row-main">
                <span className="share-title">{g.title}</span>
                <span className="share-meta">
                  <span className="meta">{g.done_tasks} of {g.tasks} tasks done</span>
                  {dueLabel(g.due_at) && <span className="meta">{dueLabel(g.due_at)}</span>}
                </span>
              </div>
              <div className="share-bar"><span style={{ width: g.tasks ? `${(100 * g.done_tasks) / g.tasks}%` : 0 }} /></div>
            </div>
          ))}
        </section>
      )}
      {view.done_recent && (
        <section className="share-panel">
          <h2>Done this week</h2>
          {view.done_recent.length === 0 && <p className="share-empty">Nothing finished yet this week.</p>}
          {view.done_recent.map((d, i) => (
            <div key={i} className="share-row done">
              <span className="share-title">{d.title}</span>
              <span className="meta">{new Date(d.completed_at).toLocaleDateString(undefined, { weekday: 'short' })}</span>
            </div>
          ))}
        </section>
      )}
    </>
  )
}

export function SharePage({ token }: { token: string }) {
  const [info, setInfo] = useState<Load<ShareInfo>>(undefined)
  const [view, setView] = useState<Load<ShareView>>(undefined)
  const [thread, setThread] = useState<ShareMessage[]>([])
  const [draft, setDraft] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const end = useRef<HTMLDivElement>(null)
  const input = useRef<HTMLTextAreaElement>(null)
  const sentOnce = useRef(false)

  const failed = (e: unknown): 'ended' | 'error' => (e instanceof ApiError && e.status === 404 ? 'ended' : 'error')

  useEffect(() => {
    api.share.info(token).then(setInfo).catch((e: unknown) => setInfo(failed(e)))
    api.share.view(token).then(setView).catch((e: unknown) => setView(failed(e)))
    api.share.messages(token).then(setThread).catch(() => setThread([]))
  }, [token])

  useEffect(() => {
    if (sentOnce.current) end.current?.scrollIntoView({ block: 'end' })
  }, [thread, busy])

  // Grows the composer with its text up to eight lines, then scrolls inside it.
  useLayoutEffect(() => {
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
  }, [draft, info])

  const send = async (e: FormEvent) => {
    e.preventDefault()
    const text = draft.trim()
    if (!text || busy) return
    sentOnce.current = true
    setBusy(true)
    setError(null)
    setDraft('')
    const asked: ShareMessage = { role: 'user', content: text, created_at: new Date().toISOString() }
    setThread((t) => [...t, asked])
    try {
      const turn = await api.share.send(token, text)
      const answered: ShareMessage = { role: 'assistant', content: turn.reply, created_at: new Date().toISOString() }
      const stored = turn.note ? await api.share.messages(token).catch(() => null) : null
      setThread((t) => stored ?? [...t, answered])
      api.share.view(token).then(setView).catch(() => {})
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

  if (info === 'ended') {
    return (
      <main className="share ended">
        <h1>This link has ended</h1>
        <p>Ask the person who shared it for a new one.</p>
      </main>
    )
  }
  if (info === undefined) return null
  if (info === 'error') {
    return (
      <main className="share ended">
        <h1>Note is not reachable</h1>
        <p>Try again in a moment.</p>
      </main>
    )
  }

  return (
    <main className="share">
      <header className="share-head">
        <h1>{info.owner}</h1>
        <p className="share-cover">{coverage(info)}</p>
      </header>
      {view && view !== 'ended' && view !== 'error' && <Panels view={view} />}
      <section className="share-chat">
        <h2>Ask Note</h2>
        <div className="share-thread" role="log" aria-live="polite">
          {thread.length === 0 && (
            <p className="share-empty">Ask what {info.owner} has today, what is done, or what is due.</p>
          )}
          {thread.map((m, i) =>
            m.role === 'note' ? (
              <div key={i} className="turn system">Sent to {info.owner}: {m.content}</div>
            ) : (
              <div key={i} className={`turn ${m.role}`}>{m.role === 'assistant' ? <Markdown text={m.content} /> : m.content}</div>
            ),
          )}
          {busy && <div className="turn pending">Note is thinking</div>}
          {error && <div className="turn system" role="alert">{error}</div>}
          <div ref={end} />
        </div>
        <form className={`tellnote${draft.trim() ? ' armed' : ''}`} onSubmit={(e) => void send(e)}>
          <textarea
            ref={input}
            rows={1}
            value={draft}
            placeholder={info.notes ? `Ask, or leave a note for ${info.owner}` : 'Ask about the plan'}
            aria-label="Ask Note"
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault()
                void send(e)
              }
            }}
          />
          <button type="submit" aria-label="Send" disabled={busy || !draft.trim()}>
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12h14" /><path d="M13 6l6 6-6 6" /></svg>
          </button>
        </form>
      </section>
    </main>
  )
}
