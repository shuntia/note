import { renderToStaticMarkup } from 'react-dom/server'
import { afterEach, expect, test, vi } from 'vitest'
import type { Mic } from '../../mic'
import { micSaver, MicRows } from './MicSection'

afterEach(() => vi.useRealTimers())

const mics: Mic[] = [
  { client: 'element', kind: 'element', label: '', gain_db: 6, auto: true },
  { client: 'web:ab12', kind: 'web', label: 'USB mic', gain_db: 12, auto: false },
]

test('each client shows its name, a 0..24 dB slider and its auto switch', () => {
  const html = renderToStaticMarkup(<MicRows mics={mics} onSlide={() => {}} onAuto={() => {}} />)
  expect([...html.matchAll(/class="set-label">([^<]*)</g)].map((m) => m[1])).toEqual(['Element X', 'USB mic'])
  const sliders = [...html.matchAll(/<input[^>]*type="range"[^>]*>/g)].map((m) => m[0])
  expect(sliders.map((s) => [/min="(\d+)"/.exec(s)?.[1], /max="(\d+)"/.exec(s)?.[1], /value="(\d+)"/.exec(s)?.[1]])).toEqual([
    ['0', '24', '6'],
    ['0', '24', '12'],
  ])
  expect([...html.matchAll(/role="switch"[^>]*aria-checked="(true|false)"/g)].map((m) => m[1])).toEqual(['true', 'false'])
})

test('a dragged slider saves its last gain once it rests, and a switch to auto drops the held save', async () => {
  vi.useFakeTimers()
  const put = vi.fn(async (client: string) => ({ ...mics[1], client }))
  const saver = micSaver(put, 300)
  saver.gain('web:ab12', 3, () => {})
  saver.gain('web:ab12', 9, () => {})
  await vi.advanceTimersByTimeAsync(300)
  expect(put.mock.calls).toEqual([['web:ab12', { gain_db: 9 }]])
  saver.gain('web:ab12', 15, () => {})
  await saver.auto('web:ab12')
  await vi.advanceTimersByTimeAsync(300)
  expect(put.mock.calls.slice(1)).toEqual([['web:ab12', { auto: true }]])
})
