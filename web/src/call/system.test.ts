import { afterEach, expect, test, vi } from 'vitest'
import { systemCall, systemMuted } from './system'

afterEach(() => {
  vi.unstubAllGlobals()
})

test('an open call holds the desktop and takes the system hang-up and mic keys until it closes', () => {
  const inCall = vi.fn()
  vi.stubGlobal('window', { noteDesktop: { ring() {}, inCall } })
  const handlers = new Map<string, (() => void) | null>()
  const setMicrophoneActive = vi.fn()
  vi.stubGlobal('navigator', {
    mediaSession: { setActionHandler: (a: string, fn: (() => void) | null) => handlers.set(a, fn), setMicrophoneActive },
  })
  const keys = { end: vi.fn(), toggleMic: vi.fn() }

  const close = systemCall(keys)
  expect(inCall).toHaveBeenLastCalledWith(true)
  handlers.get('hangup')?.()
  handlers.get('togglemicrophone')?.()
  expect(keys.end).toHaveBeenCalledOnce()
  expect(keys.toggleMic).toHaveBeenCalledOnce()
  systemMuted(true)
  expect(setMicrophoneActive).toHaveBeenLastCalledWith(false)

  close()
  expect(inCall).toHaveBeenLastCalledWith(false)
  expect(handlers.get('hangup')).toBeNull()
})

test('a browser that refuses the call actions still opens the call', () => {
  vi.stubGlobal('window', {})
  vi.stubGlobal('navigator', {
    mediaSession: {
      setActionHandler: () => {
        throw new TypeError('unsupported action')
      },
    },
  })
  expect(() => systemCall({ end() {}, toggleMic() {} })()).not.toThrow()
  expect(() => systemMuted(false)).not.toThrow()
})
