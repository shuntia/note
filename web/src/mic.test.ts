import { afterEach, expect, test } from 'vitest'
import { autoOn, gainScale, micClientId, micFrame, micKey, rememberGains, setCurrentMic, slid, type Mic } from './mic'

afterEach(() => setCurrentMic(null))

const row = (client: string, gain_db: number, auto = true): Mic => ({ client, kind: 'web', label: '', gain_db, auto })

test('a device key is the first 16 hex of the SHA-256 of its id, else its label', async () => {
  expect(await micKey({ deviceId: 'abc', label: 'ignored' })).toBe('ba7816bf8f01cfea')
  expect(await micKey({ deviceId: '', label: 'abc' })).toBe('ba7816bf8f01cfea')
  expect(await micKey({ label: 'abc' })).toBe('ba7816bf8f01cfea')
})

test('the mic frame names the client, key and a label cut to 64 characters', () => {
  const frame = JSON.parse(micFrame('desktop', 'k', 'x'.repeat(80)))
  expect(frame).toEqual({ type: 'mic', client: 'desktop', key: 'k', label: 'x'.repeat(64) })
})

test('a slid gain turns that row manual and clamps to 0..24; auto turns it back', () => {
  const rows = [row('element', 6), row('k1', 3)]
  expect(slid(rows, 'k1', 30)).toEqual([row('element', 6), row('k1', 24, false)])
  expect(slid(rows, 'k1', -2)[1].gain_db).toBe(0)
  expect(autoOn(slid(rows, 'k1', 9), 'k1')[1]).toEqual(row('k1', 9, true))
})

test("the ring scales by the current device's gain", () => {
  rememberGains([row(micClientId('web', 'aaaa'), 20), row('desktop:aaaa', 6)])
  expect(gainScale()).toBe(1)
  setCurrentMic('web:aaaa')
  expect(gainScale()).toBeCloseTo(10)
  setCurrentMic(micClientId('desktop', 'aaaa'))
  expect(gainScale()).toBeCloseTo(1.995, 3)
  setCurrentMic('web:cccc')
  expect(gainScale()).toBe(1)
})
