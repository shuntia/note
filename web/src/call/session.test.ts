import { expect, test, vi } from 'vitest'
import { CallSession, callUrl, parseFrame, type AudioIo, type Ended, type SocketLike } from './session'

class FakeSocket implements SocketLike {
  binaryType: BinaryType = 'blob'
  static startState = 1
  readyState = FakeSocket.startState
  sent: (string | ArrayBufferView)[] = []
  onmessage: SocketLike['onmessage'] = null
  onclose: SocketLike['onclose'] = null
  onopen: SocketLike['onopen'] = null
  constructor(readonly url: string) {}
  send(data: string | ArrayBufferView) {
    this.sent.push(data)
  }
  close() {
    this.readyState = 3
    this.onclose?.()
  }
  opened() {
    this.readyState = 1
    this.onopen?.()
  }
  text(v: object) {
    this.onmessage?.({ data: JSON.stringify(v) })
  }
  binary(b: ArrayBuffer) {
    this.onmessage?.({ data: b })
  }
  controls() {
    return this.sent.filter((d): d is string => typeof d === 'string').map((d) => JSON.parse(d) as unknown)
  }
  audio() {
    return this.sent.filter((d) => typeof d !== 'string').map((d) => [...(d as Int16Array)])
  }
}

type FakeAudio = AudioIo & {
  played: [number[], number][]
  flushes: number
  micStops: number
  closes: number
  isDrained: boolean
  frame(p: Int16Array): void
}

function fakeAudio(over: Partial<AudioIo> = {}): FakeAudio {
  let onFrame: ((p: Int16Array) => void) | null = null
  const a: FakeAudio = {
    played: [],
    flushes: 0,
    micStops: 0,
    closes: 0,
    isDrained: true,
    startMic: async (f) => {
      onFrame = f
    },
    stopMic: () => {
      a.micStops++
    },
    play: (p, rate) => {
      a.played.push([[...p], rate])
    },
    flush: () => {
      a.flushes++
    },
    levels: () => ({ mic: 0, out: 0 }),
    drained: () => a.isDrained,
    close: () => {
      a.closes++
    },
    frame: (p) => onFrame?.(p),
    ...over,
  }
  return a
}

function rig(over: Partial<AudioIo> = {}) {
  const audio = fakeAudio(over)
  const sockets: FakeSocket[] = []
  const seen = { states: [] as string[], captions: [] as string[], tools: [] as boolean[], ended: [] as Ended[], denied: 0 }
  const s = new CallSession(
    'ws://h/api/call/ws',
    audio,
    {
      state: (x) => seen.states.push(x),
      caption: (x) => seen.captions.push(x),
      tool: (ok) => seen.tools.push(ok),
      ended: (e) => seen.ended.push(e),
      micDenied: () => {
        seen.denied++
      },
    },
    (url) => {
      const ws = new FakeSocket(url)
      sockets.push(ws)
      return ws
    },
  )
  return { s, audio, sockets, seen }
}

test('the call url names the ring, else the thread', () => {
  const https = { protocol: 'https:', host: 'note.example' }
  expect(callUrl(https, { conversationId: 3, ring: 'r1' })).toBe('wss://note.example/api/call/ws?ring=r1')
  expect(callUrl({ protocol: 'http:', host: 'h:1' }, { conversationId: 7, ring: null })).toBe('ws://h:1/api/call/ws?conversation_id=7')
  expect(callUrl(https, { conversationId: null, ring: null })).toBe('wss://note.example/api/call/ws')
})

test('only known frames are read', () => {
  expect(parseFrame('{"type":"state","state":"thinking"}')).toEqual({ type: 'state', state: 'thinking' })
  expect(parseFrame('{"type":"agent"}')).toBeNull()
  expect(parseFrame('not json')).toBeNull()
})

test("the caller's audio goes out as binary, and as silence while muted", async () => {
  const { s, audio, sockets } = rig()
  await s.start()
  audio.frame(Int16Array.from([1, 2, 3]))
  s.setMuted(true)
  audio.frame(Int16Array.from([4, 5, 6]))
  const ws = sockets[0]
  expect(ws.binaryType).toBe('arraybuffer')
  expect(ws.audio()).toEqual([
    [1, 2, 3],
    [0, 0, 0],
  ])
  expect(ws.controls()).toEqual([{ type: 'mute', on: true }])
})

test('server frames drive the state, captions, playback and flush', async () => {
  const { s, audio, sockets, seen } = rig()
  await s.start()
  const ws = sockets[0]
  ws.text({ type: 'open', rate: 24000 })
  ws.text({ type: 'state', state: 'thinking' })
  ws.text({ type: 'caption', text: 'move my run' })
  ws.text({ type: 'tool', ok: true })
  ws.text({ type: 'tool', ok: false })
  ws.binary(Int16Array.from([7, 8]).buffer)
  ws.text({ type: 'flush' })
  expect(seen.states).toEqual(['thinking'])
  expect(seen.captions).toEqual(['move my run'])
  expect(seen.tools).toEqual([true, false])
  expect(audio.played).toEqual([[[7, 8], 24000]])
  expect(audio.flushes).toBe(1)
})

test('hanging up stops the mic but lets Note finish before the call ends', async () => {
  vi.useFakeTimers()
  try {
    const { s, audio, sockets, seen } = rig()
    await s.start()
    const ws = sockets[0]
    s.hangUp()
    expect(audio.micStops).toBe(1)
    expect(ws.controls()).toContainEqual({ type: 'hangup' })
    ws.binary(Int16Array.from([9]).buffer)
    expect(audio.played).toHaveLength(1)
    audio.isDrained = false
    ws.text({ type: 'ended', reason: 'ended', conversation_id: 4 })
    expect(seen.ended).toEqual([])
    audio.isDrained = true
    vi.advanceTimersByTime(60)
    expect(seen.ended).toEqual([{ reason: 'ended', conversationId: 4 }])
    expect(audio.closes).toBe(1)
  } finally {
    vi.useRealTimers()
  }
})

test('a closed session releases the audio even when the pending mic then fails', async () => {
  let fail!: (e: unknown) => void
  const { s, sockets, audio, seen } = rig({ startMic: () => new Promise<void>((_, j) => (fail = j)) })
  const started = s.start()
  s.close()
  const closes = audio.closes
  fail(new Error('closed'))
  await started
  expect(audio.closes).toBeGreaterThan(closes)
  expect(sockets).toHaveLength(0)
  expect(seen.ended).toEqual([])
})

test('hanging up while the mic is pending ends the call and opens no socket', async () => {
  let release!: () => void
  const { s, sockets, audio, seen } = rig({ startMic: () => new Promise<void>((r) => (release = r)) })
  const started = s.start()
  s.hangUp()
  release()
  await started
  expect(sockets).toHaveLength(0)
  expect(audio.micStops).toBeGreaterThanOrEqual(1)
  expect(audio.closes).toBeGreaterThanOrEqual(1)
  expect(seen.ended).toEqual([{ reason: 'ended', conversationId: null }])
})

test('hanging up while the socket connects ends the call', async () => {
  FakeSocket.startState = 0
  try {
    const { s, sockets, audio, seen } = rig()
    await s.start()
    s.hangUp()
    expect(sockets[0].readyState).toBe(3)
    expect(audio.micStops).toBeGreaterThanOrEqual(1)
    expect(seen.ended).toEqual([{ reason: 'ended', conversationId: null }])
  } finally {
    FakeSocket.startState = 1
  }
})

test('a mute set before the socket opens is sent when it opens', async () => {
  FakeSocket.startState = 0
  try {
    const { s, sockets } = rig()
    await s.start()
    s.setMuted(true)
    sockets[0].opened()
    expect(sockets[0].controls()).toEqual([{ type: 'mute', on: true }])
  } finally {
    FakeSocket.startState = 1
  }
})

test('a mic that cannot be read ends the call as failed', async () => {
  const { s, seen } = rig({ startMic: () => Promise.reject(new DOMException('busy', 'NotReadableError')) })
  await s.start()
  expect(seen.denied).toBe(0)
  expect(seen.ended).toEqual([{ reason: 'failed', conversationId: null }])
})

test('a denied mic opens no socket', async () => {
  const { s, sockets, seen } = rig({ startMic: () => Promise.reject(new DOMException('no', 'NotAllowedError')) })
  await s.start()
  expect(seen.denied).toBe(1)
  expect(sockets).toHaveLength(0)
})

test('audio that cannot start ends the call as failed, not as a blocked mic', async () => {
  const { s, sockets, seen } = rig({ startMic: () => Promise.reject(new Error('worklet')) })
  await s.start()
  expect(seen.denied).toBe(0)
  expect(seen.ended).toEqual([{ reason: 'failed', conversationId: null }])
  expect(sockets).toHaveLength(0)
})

test('a session closed before the mic answers opens no socket', async () => {
  let release!: () => void
  const { s, sockets, audio } = rig({ startMic: () => new Promise<void>((r) => (release = r)) })
  const started = s.start()
  s.close()
  release()
  await started
  expect(sockets).toHaveLength(0)
  expect(audio.closes).toBeGreaterThanOrEqual(1)
})

test('a dropped socket ends the call as dropped', async () => {
  const { s, sockets, seen } = rig()
  await s.start()
  sockets[0].onclose?.()
  expect(seen.ended).toEqual([{ reason: 'dropped', conversationId: null }])
})

test('an odd trailing byte of playback audio is dropped', async () => {
  const { s, audio, sockets } = rig()
  await s.start()
  sockets[0].binary(new Uint8Array([1, 0, 2, 0, 3]).buffer)
  expect(audio.played).toEqual([[[1, 2], 48000]])
})
