import { expect, test } from 'vitest'
import { paletteAt, phaseAt, placeFromZone, solarAltitude, sunTimes } from './sky'

const LA = { lat: 34.05, lon: -118.24 }
const la = (h: number, m: number) => new Date(Date.UTC(2026, 8, 25, h + 7, m)) // PDT is UTC-7

test('Los Angeles, 2026-09-25: the sun crosses the horizon near 06:48 and 18:42', () => {
  expect(solarAltitude(la(6, 45), LA.lat, LA.lon)).toBeLessThan(0)
  expect(solarAltitude(la(6, 50), LA.lat, LA.lon)).toBeGreaterThan(0)
  expect(solarAltitude(la(18, 40), LA.lat, LA.lon)).toBeGreaterThan(0)
  expect(solarAltitude(la(18, 48), LA.lat, LA.lon)).toBeLessThan(0)
  expect(solarAltitude(la(12, 0), LA.lat, LA.lon)).toBeCloseTo(53.3, 0)
})

test('a zone stands in for a place by its standard offset', () => {
  expect(placeFromZone('America/Los_Angeles')).toEqual({ lat: 35, lon: -120 })
  expect(placeFromZone('Asia/Tokyo')).toEqual({ lat: 35, lon: 135 })
})

test('phase bands', () => {
  expect(phaseAt(40)).toEqual({ from: 'day', to: 'day', t: 0 })
  expect(phaseAt(8)).toEqual({ from: 'golden', to: 'day', t: 0.5 })
  expect(phaseAt(1)).toEqual({ from: 'dusk', to: 'golden', t: 0.5 })
  expect(phaseAt(-6)).toEqual({ from: 'night', to: 'dusk', t: 0.5 })
  expect(phaseAt(-20)).toEqual({ from: 'night', to: 'night', t: 0 })
})

test('full day and full night are the keyframes themselves', () => {
  const day = paletteAt(40)
  expect(day.dark).toBe(false)
  expect(day.tokens.ink).toBe('oklch(23.00% 0.0300 255.0)')
  const night = paletteAt(-20)
  expect(night.dark).toBe(true)
  expect(night.tokens.ink).toBe('oklch(93.00% 0.0120 85.0)')
})

test('stepped tokens never blend', () => {
  const mid = paletteAt(1.5)
  expect(mid.tokens.ink).toBe(paletteAt(5).tokens.ink)
  expect(paletteAt(0.5).tokens.ink).toBe(paletteAt(-1).tokens.ink)
})

test('a midsummer night in Reykjavík never reaches night', () => {
  const d = new Date(Date.UTC(2026, 5, 21, 1, 0))
  const alt = solarAltitude(d, 64.15, -21.94)
  expect(alt).toBeGreaterThan(-10)
  expect(paletteAt(alt).phase).not.toBe('night')
})

test('sunrise and sunset, local', () => {
  const t = sunTimes(new Date(Date.UTC(2026, 8, 25, 12)), LA, 'America/Los_Angeles')
  expect(t.rise).toBe('06:48')
  expect(t.set).toBe('18:42')
})
