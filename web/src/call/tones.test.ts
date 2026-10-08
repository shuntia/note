import { expect, test } from 'vitest'
import { CUES } from './tones'

test('a call connects and hangs up with a sound, and fails without one', () => {
  expect(CUES.connect).toBeTruthy()
  expect(CUES.hangup).toBeTruthy()
  expect(CUES.failed).toBeNull()
})
