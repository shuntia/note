import { expect, test, vi } from 'vitest'
import { latest, trailing } from './coalesce'

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
