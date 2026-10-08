import { afterEach, expect, test, vi } from 'vitest'
import { ringable, setCallsOn } from './desktop'

afterEach(() => vi.unstubAllGlobals())

const page = (hidden: boolean, noteDesktop?: object) => {
  vi.stubGlobal('document', { hidden })
  vi.stubGlobal('window', noteDesktop ? { noteDesktop } : {})
}

test('a browser tab is rung only while in view', () => {
  page(false)
  expect(ringable()).toBe(true)
  page(true)
  expect(ringable()).toBe(false)
})

test('the desktop app is rung from the tray', () => {
  page(true, { ring: () => {} })
  expect(ringable()).toBe(true)
})

test('the desktop app with its calls turned off is never rung', () => {
  const kept = new Map<string, string>()
  vi.stubGlobal('localStorage', { getItem: (k: string) => kept.get(k) ?? null, setItem: (k: string, v: string) => kept.set(k, v) })
  page(false, { ring: () => {} })
  vi.stubGlobal('window', { noteDesktop: { ring: () => {} }, dispatchEvent: () => true })
  setCallsOn(false)
  expect(ringable()).toBe(false)
  setCallsOn(true)
  expect(ringable()).toBe(true)
})
