import { describe, expect, test } from 'vitest'
import { doing, receipt } from './receipts'

describe('note receipts', () => {
  test('adding quotes the note', () => {
    expect(doing('note_write', '{"op":"add","title":"call the bank"}')).toBe('Writing a note')
    expect(receipt('note_write', '{"op":"add","title":"call the bank"}', false)).toBe('Noted “call the bank”')
  })
  test('an update says what changed', () => {
    expect(receipt('note_write', '{"op":"update","id":"x","title":"milk"}', false)).toBe('Changed a note to “milk”')
    expect(receipt('note_write', '{"op":"update","id":"x","until":""}', false)).toBe('Updated a note')
  })
  test('keep, remove and failures read plainly', () => {
    expect(receipt('note_write', '{"op":"keep","id":"x"}', false)).toBe('Kept a note')
    expect(receipt('note_write', '{"op":"remove","id":"x"}', false)).toBe('Cleared a note')
    expect(receipt('note_write', '{"op":"add","title":""}', true)).toBe("Couldn't change that note")
  })
})
