import { renderToStaticMarkup } from 'react-dom/server'
import { expect, test, vi } from 'vitest'
import { previewPlayer, type Sample } from '../voicePreview'
import { RingChoices } from './Settings'

test('a_second_preview_replaces_the_first', () => {
  const audio: Sample = {
    src: '',
    play: vi.fn(() => Promise.resolve()),
    pause: vi.fn(),
  }
  const make = vi.fn(() => audio)
  const preview = previewPlayer(make)
  preview('af_heart')
  preview('bm_george')
  expect(make).toHaveBeenCalledTimes(1)
  expect(audio.pause).toHaveBeenCalledTimes(1)
  expect(audio.play).toHaveBeenCalledTimes(2)
  expect(audio.src.endsWith('bm_george')).toBe(true)
})

test('ring_for_offers_three_choices', () => {
  const html = renderToStaticMarkup(<RingChoices value="checkins" onPick={() => undefined} />)
  const buttons = [...html.matchAll(/<button[^>]*aria-pressed="(true|false)"[^>]*>([^<]*)<\/button>/g)]
  expect(buttons.map((b) => b[2])).toEqual(['Pressing', 'Check-ins', 'Never'])
  expect(buttons.map((b) => b[1])).toEqual(['false', 'true', 'false'])
})
