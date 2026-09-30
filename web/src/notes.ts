import type { Note } from './types'

export const NOTE_MAX = 200

/** The text as the server keeps it (one line, 1–200 characters), or null. */
export function noteText(raw: string): string | null {
  const text = raw.split(/\s+/).filter(Boolean).join(' ')
  const length = [...text].length
  return length > 0 && length <= NOTE_MAX ? text : null
}

/** Open notes, pinned first, then in the order they were added. */
export function openNotes(notes: Note[]): Note[] {
  return notes
    .filter((n) => n.done_at === null)
    .sort((a, b) => Number(b.pinned) - Number(a.pinned) || a.id - b.id)
}

export function withNote(notes: Note[], note: Note): Note[] {
  return notes.some((n) => n.id === note.id)
    ? notes.map((n) => (n.id === note.id ? note : n))
    : [...notes, note]
}
