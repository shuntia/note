import { expect, test, vi } from 'vitest'

vi.mock('./capture.worklet.ts?worker&url', () => ({ default: 'capture.js' }))
vi.mock('./playback.worklet.ts?worker&url', () => ({ default: 'playback.js' }))
vi.mock('./prime', () => ({
  takeAudioContext: () => ({
    state: 'running',
    audioWorklet: { addModule: () => Promise.resolve() },
    resume: () => Promise.resolve(),
    close: vi.fn(() => Promise.resolve()),
  }),
}))

const { webAudio } = await import('./audio')

test('a mic granted after the call closed is stopped at once', async () => {
  const track = { stop: vi.fn() }
  let grant!: (s: unknown) => void
  vi.stubGlobal('navigator', {
    mediaDevices: { getUserMedia: () => new Promise((r) => (grant = r)) },
  })
  const audio = webAudio()
  const started = audio.startMic(() => {})
  audio.close()
  grant({ getTracks: () => [track] })
  await expect(started).rejects.toThrow()
  expect(track.stop).toHaveBeenCalledTimes(1)
  vi.unstubAllGlobals()
})
