import { describe, expect, test } from 'vitest'
import { noteText, openNotes, withNote } from './notes'
import type { Note } from './types'

const note = (id: number, over: Partial<Note> = {}): Note => ({
  id,
  text: `n${id}`,
  pinned: false,
  created_at: '2026-09-30T12:00:00Z',
  done_at: null,
  last_nudged_at: null,
  ...over,
})

describe('noteText', () => {
  test('folds whitespace to one line', () => {
    expect(noteText('  call\n the   bank\t')).toBe('call the bank')
  })
  test('counts characters, not bytes or UTF-16 units', () => {
    expect(noteText('あ'.repeat(200))).toBe('あ'.repeat(200))
    expect(noteText('😀'.repeat(200))).toBe('😀'.repeat(200))
    expect(noteText('あ'.repeat(201))).toBeNull()
  })
  test('refuses a blank note', () => {
    expect(noteText(' \n ')).toBeNull()
  })
})

describe('openNotes', () => {
  test('drops done ones and puts pinned ones first, then the order they were added', () => {
    const list = [note(3), note(1, { pinned: true }), note(2, { done_at: '2026-09-30T13:00:00Z' }), note(4, { pinned: true })]
    expect(openNotes(list).map((n) => n.id)).toEqual([1, 4, 3])
  })
})

describe('withNote', () => {
  test('replaces a note it holds and appends one it does not', () => {
    const list = [note(1), note(2)]
    expect(withNote(list, note(2, { text: 'x' })).map((n) => n.text)).toEqual(['n1', 'x'])
    expect(withNote(list, note(3)).map((n) => n.id)).toEqual([1, 2, 3])
  })
})
