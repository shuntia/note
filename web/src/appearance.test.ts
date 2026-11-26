import { beforeEach, expect, test, vi } from 'vitest'
import { applyAppearance, DEFAULT_APPEARANCE, saveAppearance, storedAppearance } from './appearance'
import { reducedMotion } from './motion'

let osReduced = false
const attrs = new Map<string, string>()

beforeEach(() => {
  osReduced = false
  attrs.clear()
  const store = new Map<string, string>()
  vi.stubGlobal('localStorage', {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
  })
  vi.stubGlobal('document', {
    documentElement: {
      setAttribute: (k: string, v: string) => void attrs.set(k, v),
      removeAttribute: (k: string) => void attrs.delete(k),
    },
  })
  vi.stubGlobal('window', {
    matchMedia: () => ({ get matches() { return osReduced }, addEventListener: () => {} }),
  })
})

test('a stored choice outside the known set falls back to its default', () => {
  localStorage.setItem('note.appearance', JSON.stringify({ font: 'comic', text: 'larger', density: 'compact' }))
  expect(storedAppearance()).toEqual({ ...DEFAULT_APPEARANCE, text: 'larger', density: 'compact' })
})

test('defaults leave the root bare and choices mark it', () => {
  applyAppearance(DEFAULT_APPEARANCE)
  expect([...attrs.keys()]).toEqual([])
  const look = { motion: 'reduced', font: 'serif', text: 'small', density: 'compact' } as const
  saveAppearance(look)
  applyAppearance(storedAppearance())
  expect(Object.fromEntries(attrs)).toEqual({
    'data-font': 'serif',
    'data-text': 'small',
    'data-density': 'compact',
    'data-motion': 'reduced',
  })
})

test('full motion overrides the system asking for less, and system follows it', () => {
  osReduced = true
  applyAppearance({ ...DEFAULT_APPEARANCE, motion: 'full' })
  expect(reducedMotion()).toBe(false)
  expect(attrs.has('data-motion')).toBe(false)
  applyAppearance(DEFAULT_APPEARANCE)
  expect(reducedMotion()).toBe(true)
  expect(attrs.get('data-motion')).toBe('reduced')
})
