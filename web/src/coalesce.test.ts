import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'
import { latest, quiet, trailing } from './coalesce'

test('a burst becomes one call after the window', () => {
  vi.useFakeTimers()
  const fn = vi.fn()
  const bump = trailing(fn, 80)
  bump(); bump(); bump()
  vi.advanceTimersByTime(79)
  expect(fn).not.toHaveBeenCalled()
  vi.advanceTimersByTime(1)
  expect(fn).toHaveBeenCalledTimes(1)
  vi.useRealTimers()
})

test('an older response arriving late is dropped', async () => {
  const guard = latest<string>()
  let doneFirst!: (v: string) => void
  const first = guard(new Promise<string>((r) => (doneFirst = r)))
  const second = guard(Promise.resolve('two'))
  expect(await second).toBe('two')
  doneFirst('one')
  expect(await first).toBeUndefined()
})

test('an older request failing late is dropped, the newest failure still surfaces', async () => {
  const guard = latest<string>()
  let failFirst!: (e: Error) => void
  const first = guard(new Promise<string>((_, reject) => (failFirst = reject)))
  const second = guard(Promise.resolve('two'))
  expect(await second).toBe('two')
  failFirst(new Error('stale'))
  expect(await first).toBeUndefined()
  await expect(guard(Promise.reject(new Error('fresh')))).rejects.toThrow('fresh')
})

describe('quiet', () => {
  beforeEach(() => vi.useFakeTimers())
  afterEach(() => vi.useRealTimers())

  test('a run of pokes settles once, after the last one has rested', () => {
    const fn = vi.fn()
    const q = quiet(fn, 1000, 3000)
    q.poke()
    vi.advanceTimersByTime(800)
    q.poke()
    vi.advanceTimersByTime(999)
    expect(fn).not.toHaveBeenCalled()
    vi.advanceTimersByTime(1)
    expect(fn).toHaveBeenCalledTimes(1)
  })

  test('a hold keeps it waiting until released', () => {
    const fn = vi.fn()
    const q = quiet(fn, 1000, 3000)
    q.hold(true)
    q.poke()
    vi.advanceTimersByTime(1500)
    expect(fn).not.toHaveBeenCalled()
    q.hold(false)
    expect(fn).toHaveBeenCalledTimes(1)
  })

  test('a hold gives way at the cap', () => {
    const fn = vi.fn()
    const q = quiet(fn, 1000, 3000)
    q.hold(true)
    q.poke()
    vi.advanceTimersByTime(2999)
    expect(fn).not.toHaveBeenCalled()
    vi.advanceTimersByTime(1)
    expect(fn).toHaveBeenCalledTimes(1)
    q.hold(false)
    expect(fn).toHaveBeenCalledTimes(1)
  })

  test('a release before the pause has run still waits out the pause', () => {
    const fn = vi.fn()
    const q = quiet(fn, 1000, 3000)
    q.hold(true)
    q.poke()
    q.hold(false)
    expect(fn).not.toHaveBeenCalled()
    vi.advanceTimersByTime(1000)
    expect(fn).toHaveBeenCalledTimes(1)
  })

  test('cancel stops a pending call', () => {
    const fn = vi.fn()
    const q = quiet(fn, 1000, 3000)
    q.poke()
    q.cancel()
    vi.advanceTimersByTime(5000)
    expect(fn).not.toHaveBeenCalled()
  })
})
