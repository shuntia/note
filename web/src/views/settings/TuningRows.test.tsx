import { renderToStaticMarkup } from 'react-dom/server'
import { expect, test, vi } from 'vitest'
import type { Settings, Tuning } from '../../types'
import { SettingsContext } from './ToggleSetting'
import { DayBoundsRow, DeliveryRow, ReviewRow } from './TuningRows'

const tuning = {
  day_end: '00:00',
  review_day: 'thu',
  nudge_channel: 'chat',
  checkin_channel: 'push',
  review_channel: 'chat',
  brief_channel: 'push',
} as Tuning

function render(node: React.ReactNode, over: Partial<Tuning> = {}) {
  const settings = { day_start: '08:00', tuning: { ...tuning, ...over } } as Settings
  return renderToStaticMarkup(
    <SettingsContext.Provider value={{ settings, save: null, busy: false, saveSettings: vi.fn() }}>
      {node}
    </SettingsContext.Provider>,
  )
}

test('the review day is a weekday or off, and the chosen one is pressed', () => {
  const html = render(<ReviewRow open onToggle={() => undefined} />)
  expect(html.match(/<button type="button" aria-pressed/g)).toHaveLength(8)
  expect(html).toMatch(/aria-pressed="true"[^>]*>Thu</)
})

test('the day reads from its start to midnight or past it', () => {
  expect(render(<DayBoundsRow open={false} onToggle={() => undefined} />)).toContain('08:00 – Midnight')
  expect(render(<DayBoundsRow open={false} onToggle={() => undefined} />, { day_end: '03:00' })).toContain('08:00 – 03:00')
})

test('delivery counts the categories kept to the chat', () => {
  expect(render(<DeliveryRow open={false} onToggle={() => undefined} />)).toContain('2 in chat only')
})
