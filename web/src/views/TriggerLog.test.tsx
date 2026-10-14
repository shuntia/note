import { renderToStaticMarkup } from 'react-dom/server'
import { expect, test } from 'vitest'
import type { TriggerLogEntry } from '../types'
import { TriggerLog } from './TriggerLog'

const entry = (over: Partial<TriggerLogEntry>): TriggerLogEntry => ({
  event_id: 1,
  date: '2026-10-14',
  at: '09:30',
  status: 'done',
  prompt: 'see how the essay is going',
  origin: 'agent',
  outcome: null,
  text: null,
  ...over,
})

const html = (entries: TriggerLogEntry[]) => renderToStaticMarkup(<TriggerLog entries={entries} />)

test('each_trigger_reads_as_its_moment_its_prompt_and_how_it_ended', () => {
  const out = html([
    entry({ event_id: 1, outcome: 'said', text: 'How is the essay?' }),
    entry({ event_id: 2, outcome: 'quiet', text: 'they just answered' }),
    entry({ event_id: 3, status: 'dropped', outcome: 'cancelled', text: 'task_done' }),
    entry({ event_id: 4, status: 'pending', prompt: 'renew the passport' }),
  ])
  expect([...out.matchAll(/data-shape="(\w+)"/g)].map((m) => m[1])).toEqual(['spoke', 'quiet', 'off', 'waiting'])
  expect(out).toContain('see how the essay is going')
  expect(out).toContain('How is the essay?')
  expect(out).toContain('they just answered')
  expect(out).toContain('trig-end')
  expect(out).not.toContain('task_done')
  expect(out).toContain('09:30')
})

test('a_waiting_trigger_says_nothing_of_an_ending', () => {
  const out = html([entry({ status: 'pending' })])
  expect(out).not.toContain('trig-end')
  expect(out).not.toContain('trig-said')
})

test('an_empty_week_says_so', () => {
  expect(html([])).toContain('chat-empty')
})
