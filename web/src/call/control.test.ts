import { expect, test } from 'vitest'
import { CallControl, HANG_UP_WAIT_MS, LEAVE_MS, RING_MS, type CallDeps, type CallLine, type CallOpen, type Cue } from './control'
import { CallSession, type AudioIo, type CallEvents, type SocketLike } from './session'

class Socket implements SocketLike {
  binaryType: BinaryType = 'blob'
  readyState = 0
  sent: (string | ArrayBufferView)[] = []
  onmessage: SocketLike['onmessage'] = null
  onclose: SocketLike['onclose'] = null
  onopen: SocketLike['onopen'] = null
  send(data: string | ArrayBufferView) {
    this.sent.push(data)
  }
  close() {
    this.readyState = 3
  }
  opened() {
    this.readyState = 1
    this.onopen?.()
  }
}

function rig(call: CallOpen, connect?: (events: CallEvents) => CallLine) {
  const loads: (() => void)[] = []
  const timers = new Map<number, { fn: () => void; ms: number }>()
  let ids = 0
  const seen = { connects: 0, closes: 0, hangUps: 0, releases: 0, primes: 0, closed: [] as (number | null)[], blocked: 0, declined: [] as string[], rings: 0, ringing: 0, chimes: [] as Cue[] }
  const events: CallEvents[] = []
  const deps: CallDeps = {
    load: () => new Promise<void>((r) => loads.push(r)),
    connect: (e) => {
      seen.connects++
      events.push(e)
      if (connect) return connect(e)
      return {
        start: async () => {},
        setMuted: () => {},
        hangUp: () => {
          seen.hangUps++
        },
        close: () => {
          seen.closes++
        },
        levels: () => ({ mic: 0, out: 0 }),
      }
    },
    prime: () => {
      seen.primes++
    },
    release: () => {
      seen.releases++
    },
    ring: () => {
      seen.rings++
      seen.ringing++
      return () => {
        seen.ringing--
      }
    },
    chime: (cue) => seen.chimes.push(cue),
    decline: (ring) => seen.declined.push(ring),
    onClose: (id) => seen.closed.push(id),
    onMicBlocked: () => {
      seen.blocked++
    },
    later: (fn, ms) => {
      timers.set(++ids, { fn, ms })
      return ids
    },
    cancel: (id) => {
      timers.delete(id)
    },
  }
  const control = new CallControl(call, deps, () => {})
  const fire = (ms: number) => {
    for (const [id, t] of [...timers]) {
      if (t.ms !== ms) continue
      timers.delete(id)
      t.fn()
    }
  }
  const load = async () => {
    for (const r of loads) r()
    await new Promise((r) => setTimeout(r, 0))
  }
  return { control, seen, events, fire, load }
}

const started: CallOpen = { conversationId: 4, ring: null, at: 1 }
const ringing: CallOpen = { conversationId: 4, ring: 'r1', at: 1 }

test('a mute set while connecting reaches the server first and silences the mic', async () => {
  const socket = new Socket()
  let mic: (pcm: Int16Array) => void = () => {}
  const audio: AudioIo = {
    startMic: async (f) => {
      mic = f
    },
    stopMic: () => {},
    play: () => {},
    flush: () => {},
    levels: () => ({ mic: 0, out: 0 }),
    drained: () => true,
    close: () => {},
  }
  const { control, load } = rig(started, (e) => new CallSession('ws://h/api/call/ws', audio, e, () => socket))
  control.mount()
  control.tap()
  expect(control.shown.muted).toBe(true)
  await load()
  socket.opened()
  mic(Int16Array.from([5, 6, 7]))
  expect(JSON.parse(socket.sent[0] as string)).toEqual({ type: 'mute', on: true })
  expect([...(socket.sent[1] as Int16Array)]).toEqual([0, 0, 0])
})

test('mounting twice, as StrictMode does, leaves exactly one live call', async () => {
  const { control, seen, load } = rig(started)
  const cleanup = control.mount()
  cleanup()
  control.mount()
  await load()
  expect(seen.connects).toBe(1)
  expect(seen.closes).toBe(0)
  expect(seen.releases).toBe(0)
})

test('the call ends once: the audio context is released a single time and the view closes after its fade', async () => {
  const { control, seen, events, fire, load } = rig(started)
  control.mount()
  await load()
  events[0].state('listening')
  events[0].ended({ reason: 'hangup', conversationId: 4 })
  control.end()
  control.ringTaken('r1')
  expect(control.shown.phase).toBe('gone')
  expect(seen.releases).toBe(1)
  expect(seen.closed).toEqual([])
  fire(LEAVE_MS)
  expect(seen.closed).toEqual([4])
})

test('an unanswered ring gives up after its timeout', () => {
  const { control, seen, fire } = rig(ringing)
  control.mount()
  fire(RING_MS)
  expect(control.shown.phase).toBe('gone')
  expect(seen.releases).toBe(1)
  fire(LEAVE_MS)
  expect(seen.closed).toEqual([null])
})

test('a ring taken elsewhere ends this ring, and only that ring', () => {
  const { control, seen } = rig(ringing)
  control.mount()
  control.ringTaken('other')
  expect(control.shown.phase).toBe('ringing')
  control.ringTaken('r1')
  expect(control.shown.phase).toBe('gone')
  expect(seen.connects).toBe(0)
})

test('answering primes audio in the tap and connects', async () => {
  const { control, seen, load } = rig(ringing)
  control.mount()
  control.tap()
  expect(seen.primes).toBe(1)
  expect(control.shown.phase).toBe('connecting')
  await load()
  expect(seen.connects).toBe(1)
})

test('a second end while hanging up closes the call at once', async () => {
  const { control, seen, events, fire, load } = rig(started)
  control.mount()
  await load()
  events[0].state('listening')
  control.end()
  expect(control.shown.phase).toBe('hanging')
  expect(seen.hangUps).toBe(1)
  control.end()
  expect(control.shown.phase).toBe('gone')
  expect(seen.closes).toBe(1)
  expect(seen.releases).toBe(1)
  expect(seen.hangUps).toBe(1)
  fire(LEAVE_MS)
  expect(seen.closed).toEqual([4])
  fire(HANG_UP_WAIT_MS)
  expect(seen.closed).toEqual([4])
  expect(seen.releases).toBe(1)
})

test('ending a ringing call declines the ring, once', () => {
  const { control, seen } = rig(ringing)
  control.mount()
  control.end()
  control.end()
  expect(seen.declined).toEqual(['r1'])
  expect(control.shown.phase).toBe('gone')
})

test('a ring that times out, is taken elsewhere or is answered is not declined', async () => {
  const timedOut = rig(ringing)
  timedOut.control.mount()
  timedOut.fire(RING_MS)
  const taken = rig(ringing)
  taken.control.mount()
  taken.control.ringTaken('r1')
  const answered = rig(ringing)
  answered.control.mount()
  answered.control.tap()
  await answered.load()
  answered.control.end()
  for (const r of [timedOut, taken, answered]) expect(r.seen.declined).toEqual([])
})

test('a running tool shows as working and each landing is told apart from the last', async () => {
  const { control, events, load } = rig(started)
  control.mount()
  await load()
  events[0].tools(2, null)
  expect(control.shown.working).toBe(true)
  expect(control.shown.landed).toBeNull()
  events[0].tools(1, true)
  expect(control.shown.landed).toEqual({ ok: true, n: 1 })
  events[0].tools(0, false)
  expect(control.shown.working).toBe(false)
  expect(control.shown.landed).toEqual({ ok: false, n: 2 })
})

test('the ringtone sounds only while ringing, however the ring ends', async () => {
  const ends: [string, (r: ReturnType<typeof rig>) => void | Promise<void>][] = [
    ['answer', async (r) => {
      r.control.tap()
      await r.load()
    }],
    ['decline', (r) => r.control.end()],
    ['taken', (r) => r.control.ringTaken('r1')],
    ['timeout', (r) => r.fire(RING_MS)],
  ]
  for (const [, end] of ends) {
    const r = rig(ringing)
    r.control.mount()
    expect(r.seen.ringing).toBe(1)
    await end(r)
    expect(r.seen.ringing).toBe(0)
  }
})

test('a StrictMode remount leaves one ringtone going', () => {
  const { control, seen } = rig(ringing)
  control.mount()()
  control.mount()
  expect(seen.rings).toBe(2)
  expect(seen.ringing).toBe(1)
})

test('a ring that is never answered makes no call sounds', () => {
  const { control, seen } = rig(ringing)
  control.mount()
  control.end()
  expect(seen.chimes).toEqual([])
})

test('a call chimes once when it goes live and once when it ends', async () => {
  const { control, seen, events, load } = rig(started)
  control.mount()
  await load()
  events[0].state('listening')
  events[0].state('speaking')
  expect(seen.chimes).toEqual(['connect'])
  events[0].ended({ reason: 'hangup', conversationId: 4 })
  control.end()
  expect(seen.chimes).toEqual(['connect', 'hangup'])
})

test('a call that fails ends on the failure tone', async () => {
  const { control, seen, events, load } = rig(started)
  control.mount()
  await load()
  events[0].ended({ reason: 'unavailable', conversationId: null })
  expect(seen.chimes).toEqual(['failed'])
})
