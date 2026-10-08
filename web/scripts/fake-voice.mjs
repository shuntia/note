import net from 'node:net'

// Stands in for note-voice on Note's socket: answers each web call at once, shows it listening,
// counts the caller's audio frames, and ends a call when Note hangs it up.
export function fakeVoice(path) {
  const state = { up: false, audioIn: 0, calls: [], hungUp: [] }
  const seqs = new Map()
  let sock = null
  let buf = Buffer.alloc(0)
  let closed = false
  const send = (frame) => {
    const body = Buffer.from(JSON.stringify(frame))
    const head = Buffer.alloc(4)
    head.writeUInt32BE(body.length)
    sock.write(Buffer.concat([head, body]))
  }
  const call = (id, body) => {
    const seq = (seqs.get(id) ?? 0) + 1
    seqs.set(id, seq)
    send({ t: 'call', call_id: id, dir: 'to_note', seq, body })
  }
  const media = (id, body) => send({ t: 'media', call_id: id, dir: 'to_note', body })
  const on = (f) => {
    if (f.t === 'hello') state.up = true
    if (f.t === 'ping') return send({ t: 'pong', n: f.n })
    if (f.t === 'media' && f.body.k === 'audio_in') state.audioIn += 1
    if (f.t !== 'call') return
    send({ t: 'ack', call_id: f.call_id, dir: 'to_voice', seq: f.seq })
    if (f.body.k === 'start' && f.body.origin === 'web' && !state.calls.includes(f.call_id)) {
      state.calls.push(f.call_id)
      call(f.call_id, { k: 'outcome', outcome: { o: 'answered' } })
      media(f.call_id, { k: 'state', state: 'listening' })
    }
    if (f.body.k === 'hang_up' && !state.hungUp.includes(f.call_id)) {
      state.hungUp.push(f.call_id)
      call(f.call_id, { k: 'ended' })
    }
  }
  const connect = () => {
    if (closed) return
    buf = Buffer.alloc(0)
    sock = net.createConnection(path)
    sock.on('connect', () => send({ t: 'hello', proto: 4, role: 'voice', instance: 'fake' }))
    sock.on('data', (chunk) => {
      buf = Buffer.concat([buf, chunk])
      while (buf.length >= 4) {
        const n = buf.readUInt32BE(0)
        if (buf.length < 4 + n) break
        on(JSON.parse(buf.subarray(4, 4 + n).toString()))
        buf = buf.subarray(4 + n)
      }
    })
    sock.on('error', () => {})
    sock.on('close', () => {
      state.up = false
      setTimeout(connect, 200)
    })
  }
  connect()
  return {
    state,
    close: () => {
      closed = true
      sock?.destroy()
    },
  }
}
