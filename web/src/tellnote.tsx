import { useState, type FormEvent } from 'react'
import { api } from './api'
import type { ToastAction } from './app'

const KEY = 'note.lastConversation'

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
  const submit = async (e: FormEvent) => {
    e.preventDefault()
    const message = text.trim()
    if (!message || busy) return
    setBusy(true)
    try {
      const reply = await api.talk(message, lastConversation())
      rememberConversation(reply.conversation_id)
      setText('')
      notify(reply.reply.length > 90 ? `${reply.reply.slice(0, 88)}…` : reply.reply)
      onSent?.()
    } catch {
      notify("Couldn't reach Note. Try again.")
    } finally {
      setBusy(false)
    }
  }
  return (
    <form className="tellnote" onSubmit={submit}>
      <input value={text} placeholder={placeholder} aria-label={placeholder} onChange={(e) => setText(e.target.value)} />
      <button type="submit" aria-label="Send" disabled={busy || !text.trim()}>
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12h14" /><path d="M13 6l6 6-6 6" /></svg>
      </button>
    </form>
  )
}
