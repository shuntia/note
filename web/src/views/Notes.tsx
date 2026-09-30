import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react'
import { api } from '../api'
import type { ToastAction } from '../app'
import { NOTE_MAX, noteText, openNotes, withNote } from '../notes'
import { Overflow, type OverflowItem } from '../overflow'
import { Tick } from '../tick'
import type { Note, NotePatch } from '../types'
import '../styles/notes.css'

const UNDO_MS = 5000

export function Notes({
  notify,
  refresh,
}: {
  notify: (msg: string, action?: ToastAction) => void
  refresh: number
}) {
  const [notes, setNotes] = useState<Note[]>([])
  const [draft, setDraft] = useState('')
  const [editing, setEditing] = useState<{ id: number; text: string } | null>(null)
  const cancelled = useRef(false)

  const load = useCallback(() => {
    api.notes().then(setNotes).catch(() => {})
  }, [])
  useEffect(load, [load, refresh])

  const put = (note: Note) => setNotes((ns) => withNote(ns, note))
  const send = (note: Note, patch: NotePatch, failed: string) =>
    api
      .patchNote(note.id, patch)
      .then(put)
      .catch(() => {
        notify(failed)
        load()
      })

  const done = (note: Note) => {
    put({ ...note, done_at: new Date().toISOString() })
    const sent = api.patchNote(note.id, { done: true }).catch(() => {
      notify("Couldn't check that off. Try again.")
      load()
    })
    notify('Done', {
      label: 'Undo',
      run: () => {
        put(note)
        void sent.then(() => send(note, { done: false }, "Couldn't bring that back. Try again."))
      },
      windowMs: UNDO_MS,
    })
  }

  const pin = (note: Note) => {
    put({ ...note, pinned: !note.pinned })
    void send(note, { pinned: !note.pinned }, "Couldn't pin that. Try again.")
  }

  const keep = () => {
    const edit = editing
    setEditing(null)
    if (cancelled.current || !edit) {
      cancelled.current = false
      return
    }
    const note = notes.find((n) => n.id === edit.id)
    const text = noteText(edit.text)
    if (!note || !text || text === note.text) return
    put({ ...note, text })
    void send(note, { text }, "Couldn't change that note. Try again.")
  }

  const add = (e: FormEvent) => {
    e.preventDefault()
    const text = noteText(draft)
    if (!text) return
    setDraft('')
    api
      .addNote(text)
      .then(put)
      .catch(() => {
        setDraft(text)
        notify("Couldn't add that note. Try again.")
      })
  }

  const items = (note: Note): OverflowItem[] => [
    { label: 'Edit', run: () => setEditing({ id: note.id, text: note.text }) },
    { label: note.pinned ? 'Unpin' : 'Pin', run: () => pin(note) },
    { label: 'Done', run: () => done(note) },
  ]

  return (
    <div className="notes">
      <ul className="notes-list">
        {openNotes(notes).map((note) => (
          <li key={note.id} className="note-row">
            <Tick checked={false} label={`Mark ${note.text} done`} onClick={() => done(note)} />
            {editing?.id === note.id ? (
              <input
                className="note-edit"
                value={editing.text}
                maxLength={NOTE_MAX * 2}
                aria-label={`Edit ${note.text}`}
                autoFocus
                onChange={(e) => setEditing({ id: note.id, text: e.target.value })}
                onBlur={keep}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') e.currentTarget.blur()
                  else if (e.key === 'Escape') {
                    cancelled.current = true
                    e.currentTarget.blur()
                  }
                }}
              />
            ) : (
              <span className="note-text">{note.text}</span>
            )}
            {note.pinned && (
              <svg className="note-pin" viewBox="0 0 24 24" role="img" aria-label="Pinned">
                <path d="M9 4h6l-1 6 3 3H7l3-3-1-6zM12 13v7" />
              </svg>
            )}
            <Overflow
              className="note-more"
              row=".note-row"
              trigger={false}
              label={`More: ${note.text}`}
              items={items(note)}
            />
          </li>
        ))}
      </ul>
      <form className="note-new" onSubmit={add}>
        <input
          value={draft}
          maxLength={NOTE_MAX * 2}
          placeholder="+"
          aria-label="Add a note"
          enterKeyHint="done"
          onChange={(e) => setDraft(e.target.value)}
        />
      </form>
    </div>
  )
}
