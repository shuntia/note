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

describe('schedule receipts', () => {
  test('one move tool reads as a slide, a snooze or a reshape', () => {
    expect(receipt('schedule_move', '{"event_id":1,"minutes":-30}', false)).toBe('Moved an event 30 min earlier')
    expect(receipt('schedule_move', '{"event_id":1,"minutes":15,"snooze":true}', false)).toBe('Put an event off for 15 min')
    expect(receipt('schedule_move', '{"event_id":3,"start":"10:00","end":"11:00"}', false)).toBe('Reshaped a block to 10:00–11:00')
    expect(receipt('schedule_move', '{"event_id":3,"end":"11:00"}', true)).toBe("Couldn't move that event")
  })
  test('retired tools in older traces still read', () => {
    expect(receipt('schedule_snooze', '{"event_id":1,"minutes":15}', false)).toBe('Put an event off for 15 min')
    expect(receipt('wait_for', '{"until":"+30min"}', false)).toBe('Set a check-in in 30 min')
  })
})
