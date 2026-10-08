import { expect, test } from 'vitest'
import { RING_MS } from './control'
import { ATTACK_S, CUE_TAIL_MS, length, schedule, SOUNDS, voices, type Note } from './tones'

const cues = [SOUNDS.connect, SOUNDS.hangup, SOUNDS.failed]
const all: Note[] = [...schedule(SOUNDS.ring, RING_MS / 1000), ...cues.flatMap((c) => schedule(c))]

test('every voice is quiet, audible and long enough for its attack', () => {
  for (const v of voices(all)) {
    expect(v.gain).toBeGreaterThan(0)
    expect(v.gain).toBeLessThanOrEqual(0.1)
    expect(v.freq).toBeGreaterThanOrEqual(100)
    expect(v.freq).toBeLessThanOrEqual(8000)
    expect(v.dur).toBeGreaterThan(ATTACK_S * 4)
    expect(v.at).toBeGreaterThanOrEqual(0)
  }
})

test('the ring repeats its cycle each period and fills the ring, no further', () => {
  const { period, notes: cycle } = SOUNDS.ring
  const seconds = RING_MS / 1000
  const notes = schedule(SOUNDS.ring, seconds)
  const cycles = notes.length / cycle.length
  expect(Number.isInteger(cycles)).toBe(true)
  expect((cycles - 1) * period!).toBeLessThan(seconds)
  expect(cycles * period!).toBeGreaterThanOrEqual(seconds)
  notes.forEach((n, i) => {
    const c = cycle[i % cycle.length]
    expect(n.at).toBeCloseTo(Math.floor(i / cycle.length) * period! + c.at)
    expect(n.freq).toBe(c.freq)
  })
})

test('a cycle fits inside its period', () => {
  expect(length(SOUNDS.ring.notes)).toBeLessThanOrEqual(SOUNDS.ring.period!)
})

test('a cue plays once', () => {
  for (const c of cues) expect(schedule(c, 60)).toEqual(c.notes)
})

test('each partial becomes its own voice relative to its note', () => {
  const note: Note = { freq: 400, at: 0.5, dur: 1, gain: 0.05, partials: [{ ratio: 2, gain: 0.5 }] }
  expect(voices([note])).toEqual([
    { freq: 400, at: 0.5, dur: 1, gain: 0.05 },
    { freq: 800, at: 0.5, dur: 1, gain: 0.025 },
  ])
})

test('cues are short and the context outlives every one of them', () => {
  for (const c of cues) {
    expect(length(c.notes)).toBeLessThan(1.5)
    expect(CUE_TAIL_MS).toBeGreaterThan(length(c.notes) * 1000)
  }
})
