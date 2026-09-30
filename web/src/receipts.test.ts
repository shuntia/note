import { describe, expect, test } from 'vitest'
import { doing, receipt } from './receipts'

describe('note receipts', () => {
  test('adding quotes the note', () => {
    expect(doing('note_add', '{"text":"call the bank"}')).toBe('Adding a note')
    expect(receipt('note_add', '{"text":"call the bank"}', false)).toBe('Noted “call the bank”')
  })
  test('an update says what changed', () => {
    expect(receipt('note_update', '{"note_id":1,"pinned":true}', false)).toBe('Pinned a note')
    expect(receipt('note_update', '{"note_id":1,"pinned":false}', false)).toBe('Unpinned a note')
    expect(receipt('note_update', '{"note_id":1,"text":"milk"}', false)).toBe('Changed a note to “milk”')
  })
  test('done, list and failures read plainly', () => {
    expect(receipt('note_done', '{"note_id":1}', false)).toBe('Checked off a note')
    expect(receipt('note_list', '{}', false)).toBe('Read your notes')
    expect(receipt('note_add', '{"text":""}', true)).toBe("Couldn't add that note")
  })
})
