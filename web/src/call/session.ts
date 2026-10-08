import type { Levels } from './form'

export type LiveState = 'listening' | 'hearing' | 'thinking' | 'speaking'

export type ServerFrame =
  | { type: 'open'; rate: number }
  | { type: 'state'; state: LiveState }
  | { type: 'caption'; text: string }
  | { type: 'tool'; ok: boolean }
  | { type: 'flush' }
  | { type: 'ended'; reason: string; conversation_id: number | null }

const KINDS = new Set(['open', 'state', 'caption', 'tool', 'flush', 'ended'])

export function parseFrame(text: string): ServerFrame | null {
  try {
    const f = JSON.parse(text) as { type?: unknown }
    return typeof f === 'object' && f !== null && typeof f.type === 'string' && KINDS.has(f.type) ? (f as ServerFrame) : null
  } catch {
    return null
  }
}

export function callUrl(
  loc: { protocol: string; host: string },
  at: { conversationId: number | null; ring: string | null },
): string {
  const q = new URLSearchParams()
  if (at.ring) q.set('ring', at.ring)
  else if (at.conversationId !== null) q.set('conversation_id', String(at.conversationId))
  const query = q.toString()
  return `${loc.protocol === 'https:' ? 'wss' : 'ws'}://${loc.host}/api/call/ws${query ? `?${query}` : ''}`
}

export type SocketLike = {
  binaryType: BinaryType
  readyState: number
  send(data: string | ArrayBufferView): void
  close(): void
  onmessage: ((e: { data: unknown }) => void) | null
  onclose: (() => void) | null
  onopen: (() => void) | null
}

export type AudioIo = {
  startMic(onFrame: (pcm: Int16Array) => void): Promise<void>
  stopMic(): void
  play(pcm: Int16Array, rate: number): void
  flush(): void
  levels(): Levels
  drained(): boolean
  close(): void
}

export type Ended = { reason: string; conversationId: number | null }

export type CallEvents = {
  state(s: LiveState): void
  caption(text: string): void
  tool(ok: boolean): void
  ended(e: Ended): void
  micDenied(): void
}

const DRAIN_WAIT_MS = 1000
const OPEN = 1

const isMicRefusal = (e: unknown) =>
  e instanceof DOMException && (e.name === 'NotAllowedError' || e.name === 'SecurityError')

export class CallSession {
  private socket: SocketLike | null = null
  private rate = 48_000
  private muted = false
  private done = false

  constructor(
    private readonly url: string,
    private readonly audio: AudioIo,
    private readonly on: CallEvents,
    private readonly open: (url: string) => SocketLike = (u) => new WebSocket(u) as unknown as SocketLike,
  ) {}

  // The mic comes first, so a refusal ends the call before the server ever opens one.
  async start(): Promise<void> {
    try {
      await this.audio.startMic((pcm) => this.sendAudio(pcm))
    } catch (e) {
      if (this.done) {
        this.audio.close()
        return
      }
      if (isMicRefusal(e)) {
        this.done = true
        this.audio.close()
        this.on.micDenied()
      } else {
        this.finish({ reason: 'failed', conversationId: null })
      }
      return
    }
    if (this.done) {
      this.audio.close()
      return
    }
    const ws = this.open(this.url)
    ws.binaryType = 'arraybuffer'
    ws.onopen = () => this.control({ type: 'mute', on: this.muted })
    ws.onmessage = (e) => this.receive(e.data)
    ws.onclose = () => this.finish({ reason: 'dropped', conversationId: null })
    this.socket = ws
  }

  setMuted(on: boolean) {
    this.muted = on
    this.control({ type: 'mute', on })
  }

  // Once connected, Note's speech keeps playing until the server's `ended`; before that the call just ends.
  hangUp() {
    if (this.socket?.readyState !== OPEN) {
      this.finish({ reason: 'ended', conversationId: null })
      return
    }
    this.audio.stopMic()
    this.control({ type: 'hangup' })
  }

  close() {
    this.done = true
    if (this.socket) {
      this.socket.onclose = null
      this.socket.close()
    }
    this.audio.close()
  }

  levels(): Levels {
    return this.audio.levels()
  }

  private sendAudio(pcm: Int16Array) {
    if (this.socket?.readyState !== OPEN) return
    this.socket.send(this.muted ? new Int16Array(pcm.length) : pcm)
  }

  private control(msg: object) {
    if (this.socket?.readyState === OPEN) this.socket.send(JSON.stringify(msg))
  }

  private receive(data: unknown) {
    if (data instanceof ArrayBuffer) {
      this.audio.play(new Int16Array(data, 0, data.byteLength >> 1), this.rate)
      return
    }
    if (typeof data !== 'string') return
    const f = parseFrame(data)
    if (!f) return
    if (f.type === 'open') this.rate = f.rate
    else if (f.type === 'state') this.on.state(f.state)
    else if (f.type === 'caption') this.on.caption(f.text)
    else if (f.type === 'tool') this.on.tool(f.ok === true)
    else if (f.type === 'flush') this.audio.flush()
    else this.finish({ reason: f.reason, conversationId: f.conversation_id })
  }

  // Lets the browser's buffer play out Note's last words before letting go.
  private finish(e: Ended) {
    if (this.done) return
    this.done = true
    this.audio.stopMic()
    if (this.socket) {
      this.socket.onclose = null
      this.socket.close()
    }
    const started = Date.now()
    const wait = () => {
      if (this.audio.drained() || Date.now() - started >= DRAIN_WAIT_MS) {
        this.audio.close()
        this.on.ended(e)
      } else setTimeout(wait, 50)
    }
    wait()
  }
}
