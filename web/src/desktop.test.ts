import { afterEach, expect, test, vi } from 'vitest'
import { desktopRing, ringable, setCallsOn } from './desktop'

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

const ringShell = () => {
  const sent: unknown[] = []
  let act: ((a: 'answer' | 'decline') => void) | null = null
  vi.stubGlobal('window', {
    noteDesktop: {
      ring: (info?: unknown) => sent.push(['ring', info]),
      ringStop: () => sent.push(['stop']),
      onRingAction: (fn: (a: 'answer' | 'decline') => void) => {
        act = fn
        return () => {
          act = null
        }
      },
    },
  })
  return { sent, act: (a: 'answer' | 'decline') => act?.(a) }
}

test('a ring opens the popup, names the call once known and closes it on stop', async () => {
  const { sent, act } = ringShell()
  const keys = { answer: vi.fn(), decline: vi.fn() }
  const stop = desktopRing(keys, Promise.resolve('Morning check-in'))
  await Promise.resolve()
  act('answer')
  act('decline')
  stop()
  act('answer')
  expect(keys.answer).toHaveBeenCalledOnce()
  expect(keys.decline).toHaveBeenCalledOnce()
  expect(sent).toEqual([['ring', {}], ['ring', { title: 'Morning check-in' }], ['stop']])
})

test('a title arriving after the ring stopped is dropped', async () => {
  const { sent } = ringShell()
  let name: (v: string) => void = () => {}
  const stop = desktopRing({ answer() {}, decline() {} }, new Promise((r) => (name = r)))
  stop()
  name('late')
  await Promise.resolve()
  expect(sent).toEqual([['ring', {}], ['stop']])
})

test('a shell from before the popup only raises its window', () => {
  const ring = vi.fn()
  vi.stubGlobal('window', { noteDesktop: { ring } })
  desktopRing({ answer() {}, decline() {} })()
  expect(ring).toHaveBeenCalledWith()
})
