import { renderToStaticMarkup } from 'react-dom/server'
import { expect, test, vi } from 'vitest'
import type { MemoryFact } from '../types'
import { FactBody } from './Memory'

vi.mock('../markdown', () => ({ Markdown: ({ text }: { text: string }) => text }))
vi.hoisted(() => {
  globalThis.window ??= { addEventListener: () => undefined } as unknown as Window & typeof globalThis
})

const fact: MemoryFact = {
  id: '00000000-0000-4000-8000-000000000001',
  category: 'semantic',
  summary: 'likes green tea',
  body: 'no sugar',
  created: '2026-10-01T09:00:00Z',
  archived: false,
  supersedes: null,
}

const card = (f: MemoryFact, editing: 'summary' | 'body' | null = null) =>
  renderToStaticMarkup(
    <FactBody
      fact={f}
      editing={editing}
      onEdit={() => undefined}
      onSave={() => undefined}
      onForget={() => undefined}
      openTalk={() => undefined}
      cardRef={{ current: null }}
    />,
  )

test('a_live_fact_reads_as_editable_text_with_a_way_to_forget_it', () => {
  const html = card(fact)
  expect(html).toMatch(/<h2 class="memory-title" data-editable="true"/)
  expect(html).toContain('class="memory-forget"')
  expect(html).not.toContain('<textarea')
})

test('editing_opens_the_summary_and_the_body_together', () => {
  const fields = [...card(fact, 'body').matchAll(/<textarea[^>]*>([^<]*)<\/textarea>/g)].map((m) => m[1])
  expect(fields).toEqual(['likes green tea', 'no sugar'])
})

test('an_archived_fact_can_be_neither_edited_nor_forgotten', () => {
  const html = card({ ...fact, archived: true }, 'summary')
  expect(html).not.toContain('data-editable')
  expect(html).not.toContain('<textarea')
  expect(html).not.toContain('memory-forget')
})
