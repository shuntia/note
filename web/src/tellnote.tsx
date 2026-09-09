import { useEffect, useState, type FormEvent } from 'react'
import { api } from './api'
import type { ToastAction } from './app'

const KEY = 'note.lastConversation'
const REPLY_MS = 6000
const FADE_MS = 300

export function lastConversation(): number | undefined {
  try {
    const raw = localStorage.getItem(KEY)
    return raw ? Number(raw) : undefined
  } catch {
    return undefined
  }
}

export function rememberConversation(id: number) {
  try {
    localStorage.setItem(KEY, String(id))
  } catch {
    // storage blocked; the next message starts a new thread
  }
}

export function forgetConversation() {
  try {
    localStorage.removeItem(KEY)
  } catch {
    // storage blocked; nothing was remembered to clear
  }
}

export function TellNote({
  placeholder = 'Tell Note',
  notify,
  onSent,
}: {
  placeholder?: string
  notify: (msg: string, action?: ToastAction) => void
  onSent?: () => void
}) {
  const [text, setText] = useState('')
  const [busy, setBusy] = useState(false)
  // the answer belongs under the line that asked for it, and leaves on its own
  const [reply, setReply] = useState<{ text: string; at: number } | null>(null)
  const [leaving, setLeaving] = useState(false)

  useEffect(() => {
    if (!reply) return
    setLeaving(false)
    const out = window.setTimeout(() => setLeaving(true), REPLY_MS)
    const gone = window.setTimeout(() => setReply(null), REPLY_MS + FADE_MS)
    return () => {
      window.clearTimeout(out)
      window.clearTimeout(gone)
    }
  }, [reply])

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    const message = text.trim()
    if (!message || busy) return
    setBusy(true)
    try {
      const answer = await api.talk(message, lastConversation())
      rememberConversation(answer.conversation_id)
      setText('')
      setReply({ text: answer.reply, at: Date.now() })
      onSent?.()
    } catch {
      notify("Couldn't reach Note. Try again.")
    } finally {
      setBusy(false)
    }
  }
  return (
    <div className="tellnote-wrap">
      <form className="tellnote" onSubmit={submit}>
        <input value={text} placeholder={placeholder} aria-label={placeholder} onChange={(e) => setText(e.target.value)} />
        <button type="submit" aria-label="Send" disabled={busy || !text.trim()}>
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12h14" /><path d="M13 6l6 6-6 6" /></svg>
        </button>
      </form>
      {reply && (
        <div
          key={reply.at}
          className={`tellnote-reply${leaving ? ' leaving' : ''}`}
          role="status"
        >
          {reply.text}
        </div>
      )}
    </div>
  )
}
