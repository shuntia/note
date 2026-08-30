import { useEffect, useRef, useState, type FormEvent } from 'react'
import { api, ApiError } from '../api'

type Msg = { from: 'me' | 'note'; text: string }

export function Talk() {
  const [thread, setThread] = useState<Msg[]>([])
  const [draft, setDraft] = useState('')
  const [busy, setBusy] = useState(false)
  const bottom = useRef<HTMLDivElement>(null)

  useEffect(() => {
    bottom.current?.scrollIntoView({ block: 'end' })
  }, [thread])

  const send = async (e: FormEvent) => {
    e.preventDefault()
    const message = draft.trim()
    if (!message || busy) return
    setBusy(true)
    setDraft('')
    setThread((t) => [...t, { from: 'me', text: message }])
    try {
      const { reply } = await api.talk(message)
      setThread((t) => [...t, { from: 'note', text: reply }])
    } catch (err) {
      const text =
        err instanceof ApiError ? err.message : "Couldn't reach the assistant. Try again."
      setThread((t) => [...t, { from: 'note', text }])
    } finally {
      setBusy(false)
    }
  }

  return (
    <div>
      <div className="thread">
        {thread.length === 0 && <p className="muted">Say something — the assistant is listening.</p>}
        {thread.map((m, i) => (
          <div key={i} className={`bubble ${m.from}`}>
            {m.text}
          </div>
        ))}
        <div ref={bottom} />
      </div>
      <form className="composer" onSubmit={send}>
        <input
          placeholder="Say something…"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
        />
        <button className="primary" disabled={busy || !draft.trim()}>
          Send
        </button>
      </form>
    </div>
  )
}
