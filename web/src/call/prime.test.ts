import { afterEach, expect, test, vi } from 'vitest'
import { primeAudio, releaseAudioContext, takeAudioContext } from './prime'

class FakeContext {
  static made = 0
  state: AudioContextState = 'suspended'
  resumes = 0
  constructor() {
    FakeContext.made++
  }
  resume() {
    this.resumes++
    this.state = 'running'
    return Promise.resolve()
  }
  close() {
    this.state = 'closed'
    return Promise.resolve()
  }
}

vi.stubGlobal('AudioContext', FakeContext)

afterEach(() => {
  releaseAudioContext()
  FakeContext.made = 0
})

test('the context primed by the tap is the one every take gets until released', () => {
  primeAudio()
  const first = takeAudioContext()
  expect(takeAudioContext()).toBe(first)
  expect((first as unknown as FakeContext).resumes).toBe(1)
  expect(FakeContext.made).toBe(1)
  releaseAudioContext()
  expect(first.state).toBe('closed')
  expect(takeAudioContext()).not.toBe(first)
  expect(FakeContext.made).toBe(2)
})

test('priming again reuses the live context', () => {
  primeAudio()
  primeAudio()
  expect(FakeContext.made).toBe(1)
})

test('a lingering release hands out a fresh context at once and closes the old one later', () => {
  vi.useFakeTimers()
  const first = takeAudioContext()
  releaseAudioContext(500)
  expect(first.state).not.toBe('closed')
  expect(takeAudioContext()).not.toBe(first)
  vi.advanceTimersByTime(500)
  expect(first.state).toBe('closed')
  vi.useRealTimers()
})

test('a closed context is replaced', () => {
  const ctx = takeAudioContext()
  void ctx.close()
  expect(takeAudioContext()).not.toBe(ctx)
})
