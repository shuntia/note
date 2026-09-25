import { expect, test } from 'vitest'
import { zoneChange } from './zone'

const s = { timezone: 'America/Los_Angeles', timezone_auto: true, timezones: ['America/Los_Angeles', 'Asia/Tokyo'] }

test('a new device zone the server knows is a change', () => {
  expect(zoneChange(s, 'Asia/Tokyo')).toEqual({ from: 'America/Los_Angeles', to: 'Asia/Tokyo' })
})
test('the same zone, an unknown zone, or the switch off is no change', () => {
  expect(zoneChange(s, 'America/Los_Angeles')).toBeNull()
  expect(zoneChange(s, 'Asia/Calcutta')).toBeNull()
  expect(zoneChange({ ...s, timezone_auto: false }, 'Asia/Tokyo')).toBeNull()
})
test('a zoneless browser never moves the day', () => {
  const all = { ...s, timezones: [...s.timezones, 'UTC', 'Etc/UTC', 'Etc/GMT+8', 'GMT'] }
  for (const z of ['UTC', 'Etc/UTC', 'Etc/GMT+8', 'GMT']) expect(zoneChange(all, z)).toBeNull()
})
