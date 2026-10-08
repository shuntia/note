import { expect, test } from 'vitest'
import { CAPTURE_FRAME, Downsampler, JitterBuffer, Linear, rms16 } from './pcm'

const sine = (n: number, rate: number, hz: number, amp = 0.5) =>
  Float32Array.from({ length: n }, (_, i) => amp * Math.sin((2 * Math.PI * hz * i) / rate))

test('20 ms at 48 kHz becomes one 320-sample frame at 16 kHz', () => {
  const frames = new Downsampler(48_000).push(new Float32Array(960).fill(0.5))
  expect(frames).toHaveLength(1)
  expect(frames[0]).toHaveLength(CAPTURE_FRAME)
  expect(frames[0].subarray(16).every((v) => Math.abs(v - 16384) <= 1)).toBe(true)
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
  expect(loud[0].subarray(16).every((v) => v === 32767)).toBe(true)
})

const chunked = (d: Downsampler, input: Float32Array) => {
  const frames: Int16Array[] = []
  for (let i = 0; i < input.length; i += 128) frames.push(...d.push(input.subarray(i, i + 128)))
  return Int16Array.from(frames.flatMap((f) => [...f]))
}

// Least-squares fit of a sine at `hz` plus offset; returns signal-to-residual in dB.
const snrDb = (x: Int16Array, rate: number, hz: number) => {
  const w = (2 * Math.PI * hz) / rate
  let ss = 0, sc = 0, cc = 0, s1 = 0, c1 = 0, xs = 0, xc = 0, x1 = 0
  const n = x.length
  for (let i = 0; i < n; i++) {
    const s = Math.sin(w * i), c = Math.cos(w * i), v = x[i]
    ss += s * s; sc += s * c; cc += c * c; s1 += s; c1 += c; xs += v * s; xc += v * c; x1 += v
  }
  const m = [[ss, sc, s1], [sc, cc, c1], [s1, c1, n]]
  const r = [xs, xc, x1]
  for (let k = 0; k < 3; k++)
    for (let j = k + 1; j < 3; j++) {
      const f = m[j][k] / m[k][k]
      for (let l = k; l < 3; l++) m[j][l] -= f * m[k][l]
      r[j] -= f * r[k]
    }
  const c = [0, 0, 0]
  for (let k = 2; k >= 0; k--) c[k] = (r[k] - m[k].slice(k + 1).reduce((a, v, j) => a + v * c[k + 1 + j], 0)) / m[k][k]
  let sig = 0, res = 0
  for (let i = 0; i < n; i++) {
    const fit = c[0] * Math.sin(w * i) + c[1] * Math.cos(w * i)
    sig += fit * fit
    res += (x[i] - fit - c[2]) ** 2
  }
  return 10 * Math.log10(sig / res)
}

test('a 1 kHz tone comes through clean from 44.1 and 48 kHz', () => {
  for (const rate of [44_100, 48_000]) {
    const out = chunked(new Downsampler(rate), sine(rate / 2, rate, 1000))
    expect(snrDb(out.subarray(CAPTURE_FRAME), 16_000, 1000)).toBeGreaterThan(40)
  }
})

test('content above the 16 kHz band is filtered out before decimation', () => {
  const tone = chunked(new Downsampler(48_000), sine(24_000, 48_000, 12_000))
  const ref = chunked(new Downsampler(48_000), sine(24_000, 48_000, 1000))
  const db = 20 * Math.log10(rms16(ref.subarray(CAPTURE_FRAME)) / rms16(tone.subarray(CAPTURE_FRAME)))
  expect(db).toBeGreaterThanOrEqual(20)
})

test('a 96 kHz context yields 16 kHz frames', () => {
  expect(chunked(new Downsampler(96_000), sine(9600, 96_000, 300))).toHaveLength(5 * CAPTURE_FRAME)
})

test('a 44.1 kHz mic in 128-sample blocks gives the same frames as one push', () => {
  const input = sine(8820, 44_100, 700)
  const whole = Int16Array.from(new Downsampler(44_100).push(input).flatMap((f) => [...f]))
  expect([...chunked(new Downsampler(44_100), input)]).toEqual([...whole])
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

test('a backlog past three times the target drops the oldest audio down to the target', () => {
  const j = new JitterBuffer(5760, 1920)
  const out = new Float32Array(128)
  j.push(new Float32Array(12_000).fill(0.1), 0)
  expect(j.buffered).toBe(12_000)
  j.push(new Float32Array(6000).fill(0.9), 0)
  expect(j.buffered).toBe(5760)
  expect(j.pull(out, 0)).toBeCloseTo(0.9)
})
