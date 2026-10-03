import { renderToStaticMarkup } from 'react-dom/server'
import { expect, test, vi } from 'vitest'
import { previewPlayer, type Sample } from '../voicePreview'
import { VoiceGrid } from '../voiceSheet'
import { RingChoices } from './Settings'

test('a_second_preview_replaces_the_first', () => {
  const audio: Sample = {
    src: '',
    play: vi.fn(() => Promise.resolve()),
    pause: vi.fn(),
    onended: null,
  }
  const make = vi.fn(() => audio)
  const preview = previewPlayer(make)
  preview.play('af_heart')
  preview.play('bm_george')
  expect(make).toHaveBeenCalledTimes(1)
  expect(audio.pause).toHaveBeenCalledTimes(1)
  expect(audio.play).toHaveBeenCalledTimes(2)
  expect(audio.src.endsWith('bm_george')).toBe(true)
  preview.stop()
  expect(audio.pause).toHaveBeenCalledTimes(2)
})

test('ring_for_offers_three_choices', () => {
  const html = renderToStaticMarkup(<RingChoices value="checkins" onPick={() => undefined} />)
  const buttons = [...html.matchAll(/<button[^>]*aria-pressed="(true|false)"[^>]*>([^<]*)<\/button>/g)]
  expect(buttons.map((b) => b[2])).toEqual(['Pressing', 'Check-ins', 'Never'])
  expect(buttons.map((b) => b[1])).toEqual(['false', 'true', 'false'])
})

test('the_voice_grid_marks_the_chosen_and_the_playing_voice', () => {
  const voices = [
    { id: 'af_bella', label: 'Bella' },
    { id: 'bm_george', label: 'George' },
  ]
  const html = renderToStaticMarkup(
    <VoiceGrid voices={voices} chosen="bm_george" playing="af_bella" onPick={() => undefined} />,
  )
  const tiles = [...html.matchAll(/<button[^>]*aria-pressed="(true|false)"([^>]*)>/g)]
  expect(tiles.map((t) => t[1])).toEqual(['false', 'true'])
  expect(tiles.map((t) => t[2].includes('data-playing'))).toEqual([true, false])
})
