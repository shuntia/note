import { expect, test } from 'vitest'
import { BREATH_MS, ease, shape, smooth, stillForm, target } from './form'

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

test('muted opens a hairline gap at the top of the rim and dims the circle', () => {
  const f = target('listening', true)
  expect(f.dim).toBeGreaterThan(0)
  const gap = shape(f, 0, quiet).filter((p) => p.a === 0)
  expect(gap.length).toBeGreaterThan(0)
  for (const p of gap) expect(p.y).toBeLessThan(-0.95)
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
