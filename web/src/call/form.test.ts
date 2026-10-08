import { expect, test } from 'vitest'
import { BREATH_MS, MARK_MS, ease, mark, shape, smooth, stillForm, target } from './form'

const radius = (p: { x: number; y: number }) => Math.hypot(p.x, p.y)
const quiet = { mic: 0, out: 0 }

test('listening breathes two percent either way over four seconds', () => {
  const f = target('listening', false)
  expect(radius(shape(f, BREATH_MS / 4, quiet)[10])).toBeCloseTo(1.02, 3)
  expect(radius(shape(f, (3 * BREATH_MS) / 4, quiet)[10])).toBeCloseTo(0.98, 3)
})

test('with nothing written the form is one closed circle', () => {
  const pts = shape(target('hearing', false), 1234, quiet)
  for (const p of pts) expect(radius(p)).toBeCloseTo(radius(pts[0]), 6)
  expect(pts[0].x).toBeCloseTo(pts[pts.length - 1].x, 6)
  expect(pts[0].y).toBeCloseTo(pts[pts.length - 1].y, 6)
})

test("the caller's voice ripples the rim", () => {
  const r = shape(target('hearing', false), 500, { mic: 1, out: 0 }).map(radius)
  expect(Math.max(...r) - Math.min(...r)).toBeGreaterThan(0.03)
})

test('speaking swells with what is playing', () => {
  const f = target('speaking', false)
  expect(radius(shape(f, 0, { mic: 0, out: 1 })[0])).toBeGreaterThan(radius(shape(f, 0, quiet)[0]) + 0.05)
})

test('thinking writes one stroke across the centre, inked at the nib and fading behind it', () => {
  const pts = shape(target('thinking', false), 2000, quiet)
  expect(Math.min(...pts.map(radius))).toBeLessThan(0.25)
  expect(pts[pts.length - 1].a).toBe(1)
  expect(pts[0].a).toBeLessThan(0.05)
})

test('a change of state moves the form smoothly, never in a jump', () => {
  let f = target('listening', false)
  let prev = shape(f, 0, quiet)
  for (let t = 1; t <= 600; t++) {
    f = ease(f, target(t < 300 ? 'thinking' : 'speaking', false), 1)
    const next = shape(f, t, { mic: 0, out: 0.5 })
    const moved = Math.max(...next.map((p, i) => Math.hypot(p.x - prev[i].x, p.y - prev[i].y)))
    expect(moved).toBeLessThan(0.05)
    prev = next
  }
})

test('a state is reached within about 400 ms', () => {
  let f = target('listening', false)
  for (let i = 0; i < 25; i++) f = ease(f, target('thinking', false), 16)
  expect(f.write).toBeGreaterThan(0.94)
})

test('muted opens a gap of about fifteen degrees at the top of the rim and dims the circle', () => {
  const f = target('listening', true)
  expect(f.dim).toBeGreaterThan(0)
  const gap = shape(f, 0, quiet).filter((p) => p.a === 0)
  expect(gap.length).toBeGreaterThan(0)
  for (const p of gap) expect(p.y).toBeLessThan(-0.95)
  const half = shape(f, 0, quiet, 720).filter((p) => p.a < 0.5)
  const arc = (half.length * 360) / 719
  expect(arc).toBeGreaterThan(13)
  expect(arc).toBeLessThan(17)
})

test('levels rise fast and fall slowly', () => {
  expect(smooth(0, 1, 30)).toBeGreaterThan(0.6)
  expect(smooth(1, 0, 30)).toBeGreaterThan(0.85)
})

test('reduced motion holds a still circle, dimmed while Note thinks', () => {
  const still = stillForm('thinking', false)
  expect(still.write).toBe(0)
  expect(still.dim).toBeGreaterThan(0)
  for (const p of shape(still, 0, { mic: 1, out: 1 })) expect(radius(p)).toBeCloseTo(1, 6)
})

test('the mute gap fades in and turns with the rim without any point flickering', () => {
  let f = target('listening', false)
  let prev = shape(f, 0, quiet)
  for (let t = 16; t <= 3000; t += 16) {
    f = ease(f, target('listening', true), 16)
    const next = shape(f, t, quiet)
    for (let i = 0; i < next.length; i++) expect(Math.abs(next[i].a - prev[i].a)).toBeLessThan(0.15)
    prev = next
  }
})

test('easing ignores a backwards, zero or broken frame time and settles on a huge one', () => {
  const from = target('listening', false)
  const to = target('thinking', false)
  expect(ease(from, to, -50)).toEqual(from)
  expect(ease(from, to, 0)).toEqual(from)
  expect(ease(from, to, NaN)).toEqual(from)
  expect(ease(from, to, Infinity)).toEqual(from)
  expect(ease(from, to, 1e9)).toEqual(to)
})

test('smoothing ignores a backwards, zero or broken frame time and a broken level', () => {
  expect(smooth(0.5, 1, -50)).toBe(0.5)
  expect(smooth(0.5, 1, 0)).toBe(0.5)
  expect(smooth(0.5, 1, NaN)).toBe(0.5)
  expect(smooth(0.5, 1, 1e9)).toBe(1)
  expect(smooth(0.5, NaN, 1e9)).toBe(0)
  expect(Number.isFinite(smooth(0.5, Infinity, 16))).toBe(true)
})

test('easing and smoothing do not depend on the frame rate', () => {
  const run = (steps: number, dt: number) => {
    let f = target('listening', false)
    let lv = 0
    for (let i = 0; i < steps; i++) {
      f = ease(f, target('thinking', false), dt)
      lv = smooth(lv, 1, dt)
    }
    return { f, lv }
  }
  const a = run(25, 16)
  for (const b of [run(50, 8), run(400, 1)]) {
    expect(Math.abs(b.lv - a.lv)).toBeLessThan(1e-9)
    for (const key of Object.keys(a.f) as (keyof typeof a.f)[]) expect(Math.abs(b.f[key] - a.f[key])).toBeLessThan(1e-9)
  }
})

const settled = (look: 'thinking' | 'hearing') => {
  let f = target('listening', false)
  for (let i = 0; i < 200; i++) f = ease(f, target(look, false), 16)
  return f
}

test('the voice shimmers the rim in fine travelling ripples and it stays a circle', () => {
  const r = shape(settled('hearing'), 777, { mic: 1, out: 0 }, 720).map(radius)
  const mean = r.reduce((x, y) => x + y, 0) / r.length
  expect(Math.max(...r.map((x) => Math.abs(x - mean)))).toBeLessThanOrEqual(0.04 * mean + 1e-9)
  let crossings = 0
  for (let i = 1; i < r.length; i++) if (Math.sign(r[i] - mean) !== Math.sign(r[i - 1] - mean)) crossings++
  expect(crossings).toBeGreaterThanOrEqual(14)
})

test('the written line keeps within the width of the circle, its nib in the right third', () => {
  const f = settled('thinking')
  for (let t = 0; t < 20_000; t += 137) {
    const pts = shape(f, t, quiet)
    for (const p of pts) if (p.a > 0.01) expect(Math.abs(p.x)).toBeLessThanOrEqual(1)
    expect(pts[pts.length - 1].x).toBeGreaterThan(0.33)
  }
})

test('the ink fades smoothly from the nib to the tail with no break', () => {
  const pts = shape(settled('thinking'), 4321, quiet)
  for (let i = 1; i < pts.length; i++) {
    expect(Math.abs(pts[i].a - pts[i - 1].a)).toBeLessThan(0.05)
    expect(pts[i].a).toBeGreaterThanOrEqual(pts[i - 1].a)
  }
})

test('the line is written loop after loop, sliding left without ever jumping back', () => {
  const f = settled('thinking')
  let prev = shape(f, 0, quiet)
  let nibUp = 0
  for (let t = 16; t <= 12_000; t += 16) {
    const next = shape(f, t, quiet)
    const moved = Math.max(...next.map((p, i) => Math.hypot(p.x - prev[i].x, p.y - prev[i].y)))
    expect(moved).toBeLessThan(0.05)
    if (next[next.length - 1].y < -0.15 && prev[prev.length - 1].y >= -0.15) nibUp++
    prev = next
  }
  expect(nibUp).toBeGreaterThanOrEqual(10)
})

test('at 60 frames a second the circle unrolls into the line and curls back without a pop', () => {
  let f = target('listening', false)
  let prev = shape(f, 0, quiet)
  for (let t = 16; t <= 2400; t += 16) {
    f = ease(f, target(t < 1200 ? 'thinking' : 'speaking', false), 16)
    const next = shape(f, t, { mic: 0, out: 0.5 })
    for (let i = 0; i < next.length; i++) {
      expect(Math.hypot(next[i].x - prev[i].x, next[i].y - prev[i].y)).toBeLessThan(0.35)
      expect(Math.abs(next[i].a - prev[i].a)).toBeLessThan(0.3)
    }
    prev = next
  }
})

test('while a tool runs the rim grows teeth that turn', () => {
  const r = (t: number) => shape(target('listening', false, true), t, quiet).map(radius)
  const at0 = r(0)
  expect(Math.max(...at0) - Math.min(...at0)).toBeGreaterThan(0.08)
  expect(r(400)[20]).not.toBeCloseTo(at0[20], 3)
  expect(target('ending', false, true).gear).toBe(0)
})

const ends = (m: { strokes: { x: number; y: number }[][] } | null) =>
  (m?.strokes ?? []).map((l) => [l[0], l[l.length - 1]])

test('a tool that worked writes a check mark on, then erases it from where it began', () => {
  const writing = mark(true, 200)!.strokes[0]
  const whole = mark(true, 520)!.strokes[0]
  const erasing = mark(true, 800)!.strokes[0]
  expect(writing[0]).toEqual(whole[0])
  expect(whole).toHaveLength(3)
  expect(erasing[erasing.length - 1]).toEqual(whole[whole.length - 1])
  expect(erasing[0]).not.toEqual(whole[0])
  expect(mark(true, MARK_MS)).toBeNull()
})

test('a tool that failed writes an X stroke by stroke and erases the strokes in the same order', () => {
  expect(mark(false, 150)!.strokes).toHaveLength(1)
  const both = ends(mark(false, 620))
  expect(both).toHaveLength(2)
  const firstGoing = ends(mark(false, 780))
  expect(firstGoing[0][1]).toEqual(both[0][1])
  expect(firstGoing[0][0]).not.toEqual(both[0][0])
  expect(firstGoing[1]).toEqual(both[1])
  const second = ends(mark(false, 1000))
  expect(second).toHaveLength(1)
  expect(second[0][1]).toEqual(both[1][1])
  expect(mark(false, -1)).toBeNull()
})
