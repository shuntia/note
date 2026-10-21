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

describe('memory receipts', () => {
  test('memory_find reads as a search or a read', () => {
    expect(receipt('memory_find', '{"query":"tea"}', false)).toBe('Searched memory for “tea”')
    expect(receipt('memory_find', '{"id":"0f0e"}', false)).toBe('Read a saved note')
  })
  test('memory_save reads as the note or memory it wrote', () => {
    expect(doing('memory_save', '{"items":[{"kind":"note","title":"call the bank"}]}')).toBe('Writing a note')
    expect(receipt('memory_save', '{"items":[{"kind":"note","title":"call the bank"}]}', false)).toBe('Noted “call the bank”')
    expect(receipt('memory_save', '{"items":[{"id":"1a2b3c4d"}]}', false)).toBe('Kept a note')
    expect(receipt('memory_save', '{"items":[{"id":"1a2b3c4d","title":"milk"}]}', false)).toBe('Changed a note to “milk”')
    expect(receipt('memory_save', '{"items":[{"id":"1a2b3c4d","kind":"semantic"}]}', false)).toBe('Kept a note as a memory')
    expect(receipt('memory_save', '{"items":[{"title":"likes tea"}]}', false)).toBe('Remembered: likes tea')
    expect(receipt('memory_save', '{"items":[{"supersedes":"x","title":"coffee now"}]}', false)).toBe('Replaced an older note: coffee now')
    expect(receipt('memory_save', '{"items":[{"title":"a"},{"title":"b"}]}', false)).toBe('Saved 2 items to memory')
    expect(receipt('memory_save', '{"items":[]}', true)).toBe("Couldn't save that to memory")
  })
  test('memory_remove counts what it cleared', () => {
    expect(receipt('memory_remove', '{"ids":["a","b"]}', false)).toBe('Cleared 2 items from memory')
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

describe('find, save and remove receipts', () => {
  test('task_save reads as the create or update it carries', () => {
    expect(receipt('task_save', '{"tasks":[{"title":"Essay"}]}', false)).toBe('Added “Essay” to your tasks')
    expect(receipt('task_save', '{"tasks":[{"id":4,"state":"done"}]}', false)).toBe('Marked a task done')
    expect(receipt('task_save', '{"tasks":[{"id":4,"now":true}]}', false)).toBe(receipt('task_update', '{"is_now":true}', false))
    expect(receipt('task_save', '{"tasks":[{"title":"a"},{"title":"b"}]}', false)).toBe('Saved 2 tasks')
    expect(doing('task_save', '{}')).toBe('Saving tasks')
    expect(receipt('task_save', '{}', true)).toBe("Couldn't save those tasks")
  })
  test('task_find reads as a search, a read or a survey', () => {
    expect(receipt('task_find', '{"query":"essay"}', false)).toBe('Looked for “essay” in your tasks')
    expect(receipt('task_find', '{"id":3}', false)).toBe('Read a task')
    expect(receipt('task_find', '{"overdue":true}', false)).toBe('Looked over what is overdue')
  })
  test('the day tools read as the change they carry', () => {
    expect(receipt('day_save', '{"items":[{"task":4,"at":"14:00"}]}', false)).toBe('Laid 1 task onto today from 14:00')
    expect(receipt('day_save', '{"items":[{"wake":"see whether the draft started","at":"+30min"}]}', false)).toBe('Set a check-in in 30 min')
    expect(receipt('day_save', '{"replace":true,"items":[{"task":1},{"task":2}]}', false)).toBe('Put 2 tasks in order')
    expect(receipt('day_save', '{"carry":true}', false)).toBe('Carried the rest of today over')
    expect(receipt('day_save', '{"items":[1,2,3]}', false)).toBe('Saved 3 items to today')
    expect(receipt('day_save', '{"items":[]}', true)).toBe("Couldn't change the day")
    expect(receipt('day_remove', '{"ids":["t4"]}', false)).toBe('Took an item off the day')
    expect(receipt('day_find', '{}', false)).toBe("Looked at today's plan")
  })
  test('removals, goals and the calendar read plainly', () => {
    expect(receipt('task_remove', '{"ids":[1]}', false)).toBe('Deleted a task')
    expect(receipt('task_remove', '{"ids":[1,2,3]}', false)).toBe('Deleted 3 tasks')
    expect(receipt('goal_save', '{"goals":[{"title":"Visual Arts"}]}', false)).toBe('Saved the goal “Visual Arts”')
    expect(receipt('goal_find', '{}', false)).toBe('Read your goals')
    expect(receipt('calendar_save', '{"entries":[{"title":"Swim"}]}', false)).toBe('Added “Swim” to your calendar')
    expect(receipt('calendar_save', '{"entries":[{"id":2,"skip":["2026-11-03"]}]}', false)).toMatch(/^Skipped a calendar entry/)
    expect(receipt('calendar_find', '{}', false)).toBe('Read your calendar')
  })
})
