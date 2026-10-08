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

describe('order receipts', () => {
  test('each order tool reads plainly', () => {
    expect(receipt('order_set', '{"task_ids":[1,2]}', false)).toBe('Put 2 tasks in order')
    expect(receipt('order_move', '{"task_id":1}', false)).toBe('Moved a task in the order')
    expect(receipt('order_drop', '{"task_id":1}', false)).toBe('Took a task out of the order')
    expect(receipt('order_set', '{"task_ids":[]}', true)).toBe("Couldn't set the order")
  })
})
