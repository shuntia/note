import { afterEach, expect, test, vi } from 'vitest'
import { ringable } from './desktop'

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
