import { expect, test } from 'vitest'
import { outageLines } from './outages'

test('each_standing_outage_reads_as_its_line_and_an_unknown_one_is_left_out', () => {
  const lines = outageLines([
    { code: 'provider_down', since: 'x' },
    { code: 'daily_cap', since: 'x' },
    { code: 'tts_down' as 'voice_down', since: 'x' },
  ])
  expect(lines).toEqual([
    "The AI service isn't responding. Try again shortly.",
    "Today's AI sessions are used up. Wake-ups go out as plain reminders.",
  ])
})
