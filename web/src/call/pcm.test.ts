import { expect, test } from 'vitest'
import { CAPTURE_FRAME, Downsampler, JitterBuffer, Linear, rms16 } from './pcm'

const sine = (n: number, rate: number, hz: number, amp = 0.5) =>
  Float32Array.from({ length: n }, (_, i) => amp * Math.sin((2 * Math.PI * hz * i) / rate))

test('20 ms at 48 kHz becomes one 320-sample frame at 16 kHz', () => {
  const frames = new Downsampler(48_000).push(new Float32Array(960).fill(0.5))
  expect(frames).toHaveLength(1)
  expect(frames[0]).toHaveLength(CAPTURE_FRAME)
  expect(frames[0].every((v) => v === 16384)).toBe(true)
})

test('the mic in 128-sample blocks gives the same frames as one push', () => {
  const input = sine(4800, 48_000, 440)
  const whole = new Downsampler(48_000).push(input)
  const d = new Downsampler(48_000)
  const parts: Int16Array[] = []
  for (let i = 0; i < input.length; i += 128) parts.push(...d.push(input.subarray(i, i + 128)))
  expect(parts.map((f) => [...f])).toEqual(whole.map((f) => [...f]))
})

test('a 44.1 kHz context still yields 16 kHz frames', () => {
  const d = new Downsampler(44_100)
  const input = sine(4420, 44_100, 300)
  const frames: Int16Array[] = []
  for (let i = 0; i < input.length; i += 128) frames.push(...d.push(input.subarray(i, i + 128)))
  expect(frames).toHaveLength(5)
})

test('a voice-band tone keeps its level, and loud input clips instead of wrapping', () => {
  const out = new Downsampler(48_000).push(sine(9600, 48_000, 440))
  const level = out.reduce((s, f) => s + rms16(f), 0) / out.length
  expect(level).toBeGreaterThan((0.5 / Math.SQRT2) * 0.9)
  expect(level).toBeLessThan((0.5 / Math.SQRT2) * 1.05)
  const loud = new Downsampler(48_000).push(new Float32Array(960).fill(1.5))
  expect(loud[0].every((v) => v === 32767)).toBe(true)
})

test('playback at the context rate is a plain conversion', () => {
  expect([...new Linear(48_000, 48_000).push(Int16Array.from([0, 16384, -32768]))]).toEqual([0, 0.5, -1])
})

test('48 kHz played on a 44.1 kHz context keeps its duration', () => {
  const n = new Linear(48_000, 44_100).push(new Int16Array(4800)).length
  expect(n).toBeGreaterThanOrEqual(4409)
  expect(n).toBeLessThanOrEqual(4411)
})

test('resampling carries its position across chunks', () => {
  const input = Int16Array.from({ length: 960 }, (_, i) => Math.round(8000 * Math.sin(i / 7)))
  const whole = [...new Linear(48_000, 24_000).push(input)]
  const l = new Linear(48_000, 24_000)
  const parts = [...l.push(input.subarray(0, 333)), ...l.push(input.subarray(333))]
  expect(parts).toHaveLength(whole.length)
  parts.forEach((v, i) => expect(v).toBeCloseTo(whole[i], 6))
})

test('playback holds until 120 ms is buffered', () => {
  const j = new JitterBuffer(5760, 1920)
  const out = new Float32Array(128)
  j.push(new Float32Array(4800).fill(0.25), 0)
  expect(j.pull(out, 128)).toBe(0)
  expect(out.every((v) => v === 0)).toBe(true)
  j.push(new Float32Array(960).fill(0.25), 128)
  expect(j.pull(out, 256)).toBeCloseTo(0.25)
  expect(out[0]).toBe(0.25)
})

test('a short last phrase plays once nothing more arrives', () => {
  const j = new JitterBuffer(5760, 1920)
  const out = new Float32Array(128)
  j.push(new Float32Array(2400).fill(0.5), 0)
  expect(j.pull(out, 1000)).toBe(0)
  expect(j.pull(out, 1920)).toBeCloseTo(0.5)
})

test('a flush drops everything and waits to fill again', () => {
  const j = new JitterBuffer(5760, 1920)
  const out = new Float32Array(128)
  j.push(new Float32Array(6000).fill(0.5), 0)
  expect(j.pull(out, 0)).toBeGreaterThan(0)
  j.flush()
  expect(j.buffered).toBe(0)
  j.push(new Float32Array(100).fill(0.5), 10)
  expect(j.pull(out, 20)).toBe(0)
})

test('running dry pads with silence and waits to fill again', () => {
  const j = new JitterBuffer(5760, 1920)
  const out = new Float32Array(128)
  j.push(new Float32Array(5800).fill(0.5), 0)
  for (let i = 0; i < 45; i++) j.pull(out, 0)
  j.pull(out, 0)
  expect(out[39]).toBe(0.5)
  expect(out[40]).toBe(0)
  j.push(new Float32Array(128).fill(0.5), 0)
  expect(j.pull(out, 10)).toBe(0)
})
