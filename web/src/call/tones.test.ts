import { expect, test } from 'vitest'
import { RING_MS } from './control'
import { ATTACK_S, CUE_TAIL_MS, CUES, length, RING_PERIOD_S, ringNotes, type Note } from './tones'

const all: Note[] = [...ringNotes(RING_MS / 1000), ...Object.values(CUES).flat()]

test('every note is quiet, audible and long enough for its attack', () => {
  for (const n of all) {
    expect(n.peak).toBeGreaterThan(0)
    expect(n.peak).toBeLessThanOrEqual(0.1)
    expect(n.freq).toBeGreaterThanOrEqual(200)
    expect(n.freq).toBeLessThanOrEqual(2000)
    expect(n.dur).toBeGreaterThan(ATTACK_S * 4)
    expect(n.at).toBeGreaterThanOrEqual(0)
  }
})

test('the ringtone repeats each period and fills the ring, no further', () => {
  const notes = ringNotes(RING_MS / 1000)
  const firsts = notes.filter((_, i) => i % 2 === 0).map((n) => n.at)
  expect(firsts[0]).toBe(0)
  for (let i = 1; i < firsts.length; i++) expect(firsts[i] - firsts[i - 1]).toBeCloseTo(RING_PERIOD_S)
  expect(firsts.at(-1)).toBeLessThan(RING_MS / 1000)
  expect(firsts.at(-1)! + RING_PERIOD_S).toBeGreaterThanOrEqual(RING_MS / 1000)
})

test('connecting rises, ending falls, and a failure falls lower', () => {
  const freqs = (notes: Note[]) => notes.map((n) => n.freq)
  const [c0, c1] = freqs(CUES.connected)
  const [e0, e1] = freqs(CUES.ended)
  const [f0, f1] = freqs(CUES.failed)
  expect(c1).toBeGreaterThan(c0)
  expect(e1).toBeLessThan(e0)
  expect(f1).toBeLessThan(f0)
  expect(f1).toBeLessThan(e1)
})

test('cues are short and the context outlives every one of them', () => {
  for (const notes of Object.values(CUES)) {
    expect(length(notes)).toBeLessThan(1)
    expect(CUE_TAIL_MS).toBeGreaterThan(length(notes) * 1000)
  }
})
