import { expect, test } from 'vitest'
import { CallControl, LEAVE_MS, RING_MS, type CallDeps, type CallLine, type CallOpen } from './control'
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
  const seen = { connects: 0, closes: 0, releases: 0, primes: 0, closed: [] as (number | null)[], blocked: 0 }
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
        hangUp: () => {},
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
