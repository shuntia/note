import { afterEach, expect, test } from 'vitest'
import {
  autoOn,
  gainScale,
  micClientId,
  micFrame,
  micInputs,
  micKey,
  openMic,
  realDevice,
  rememberGains,
  setCurrentMic,
  slid,
  type Mic,
} from './mic'

afterEach(() => setCurrentMic(null))

const row = (client: string, gain_db: number, auto = true): Mic => ({ client, kind: 'web', label: '', gain_db, auto })

test('a device key is the first 16 hex of the SHA-256 of its id, else its label', async () => {
  expect(await micKey({ deviceId: 'abc', label: 'ignored' })).toBe('ba7816bf8f01cfea')
  expect(await micKey({ deviceId: '', label: 'abc' })).toBe('ba7816bf8f01cfea')
  expect(await micKey({ label: 'abc' })).toBe('ba7816bf8f01cfea')
})

test("an aliased default input is keyed by the real device behind it", () => {
  const inputs = [
    { deviceId: 'default', groupId: 'g2', label: 'Default - Headset' },
    { deviceId: 'communications', groupId: 'g2', label: 'Communications - Headset' },
    { deviceId: 'laptop-id', groupId: 'g1', label: 'Built-in' },
    { deviceId: 'headset-id', groupId: 'g2', label: 'Headset' },
  ]
  expect(realDevice({ deviceId: 'default', groupId: 'g2', label: 'Default - Headset' }, inputs).deviceId).toBe('headset-id')
  expect(realDevice({ deviceId: 'communications', groupId: 'g1', label: 'x' }, inputs).deviceId).toBe('laptop-id')
  expect(realDevice({ deviceId: 'default', label: 'Default - Built-in' }, inputs).deviceId).toBe('laptop-id')
  expect(realDevice({ deviceId: 'default', groupId: 'g9', label: 'Gone' }, []).deviceId).toBe('g9')
  expect(realDevice({ deviceId: 'usb-id', groupId: 'g3', label: 'USB' }, inputs).deviceId).toBe('usb-id')
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

const stream = {} as MediaStream
const failing = (name: string) => Object.assign(new Error(name), { name })

test('the chosen input opens by its exact id', async () => {
  const asked: MediaStreamConstraints[] = []
  const got = await openMic({ channelCount: 1 }, async (c) => (asked.push(c), stream), 'usb')
  expect(got).toBe(stream)
  expect(asked).toEqual([{ audio: { channelCount: 1, deviceId: { exact: 'usb' } } }])
})

test('a chosen input that is gone falls back to the default', async () => {
  const asked: MediaStreamConstraints[] = []
  const open = async (c: MediaStreamConstraints) => {
    asked.push(c)
    if (asked.length === 1) throw failing('OverconstrainedError')
    return stream
  }
  expect(await openMic({}, open, 'gone')).toBe(stream)
  expect(asked[1]).toEqual({ audio: {} })
})

test('a refused mic is not retried', async () => {
  let tries = 0
  const open = async () => {
    tries++
    throw failing('NotAllowedError')
  }
  await expect(openMic({}, open, 'usb')).rejects.toThrow('NotAllowedError')
  expect(tries).toBe(1)
})

test('no choice opens the default', async () => {
  const asked: MediaStreamConstraints[] = []
  await openMic({}, async (c) => (asked.push(c), stream), null)
  expect(asked).toEqual([{ audio: {} }])
})

test('the input list leaves out the default aliases and outputs', () => {
  const list = micInputs([
    { kind: 'audioinput', deviceId: 'default', label: 'Default - USB' },
    { kind: 'audioinput', deviceId: 'communications', label: 'Comms' },
    { kind: 'audioinput', deviceId: 'usb', label: 'USB' },
    { kind: 'audiooutput', deviceId: 'spk', label: 'Speakers' },
  ])
  expect(list).toEqual([{ deviceId: 'usb', label: 'USB' }])
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
