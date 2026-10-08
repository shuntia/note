import type { Levels } from './form'
import type { CallEvents, LiveState } from './session'

export type CallOpen = { conversationId: number | null; ring: string | null; at: number }
export type Phase = 'ringing' | 'connecting' | 'live' | 'hanging' | 'gone'
export type Cue = 'connect' | 'hangup' | 'failed'
export type CallShown = {
  phase: Phase
  live: LiveState
  muted: boolean
  caption: { text: string; n: number } | null
  working: boolean
  landed: { ok: boolean; n: number } | null
  shake: boolean
}

export type CallLine = {
  start(): Promise<void>
  setMuted(on: boolean): void
  hangUp(): void
  close(): void
  levels(): Levels
}

export type CallDeps = {
  load(): Promise<void>
  connect(events: CallEvents): CallLine
  prime(): void
  release(): void
  // Starts the ringtone; the returned function stops it.
  ring(): () => void
  chime(cue: Cue): void
  decline(ring: string): void
  onClose(conversationId: number | null): void
  onMicBlocked(): void
  later(fn: () => void, ms: number): number
  cancel(id: number): void
}

export const LEAVE_MS = 450
export const RING_MS = 30_000
// Past the voice side's longest drain, so Note's last words always finish first.
export const HANG_UP_WAIT_MS = 62_000
const FAILED = new Set(['unavailable', 'busy', 'failed', 'missed'])

type Run = { alive: boolean; line: CallLine | null }

// The call view's behaviour without the DOM: `mount` may run twice (StrictMode) and only one line survives;
// `leave` is the call's one true end: it sounds the end of a call that was answered and releases the primed audio context.
export class CallControl {
  shown: CallShown
  private run: Run | null = null
  private timers: number[] = []
  private captions = 0
  private landings = 0
  private ringtone: (() => void) | null = null

  constructor(
    private readonly call: CallOpen,
    private readonly deps: CallDeps,
    private readonly changed: (s: CallShown) => void,
  ) {
    this.shown = { phase: call.ring ? 'ringing' : 'connecting', live: 'listening', muted: false, caption: null, working: false, landed: null, shake: false }
  }

  mount(): () => void {
    if (this.call.ring) {
      this.ringtone = this.deps.ring()
      const ring = this.deps.later(() => {
        if (this.shown.phase === 'ringing') this.leave(null, false)
      }, RING_MS)
      return () => {
        this.deps.cancel(ring)
        this.silence()
        this.stop()
      }
    }
    void this.begin()
    return () => this.stop()
  }

  dispose() {
    for (const id of this.timers) this.deps.cancel(id)
    this.timers = []
  }

  levels(): Levels {
    return this.run?.line?.levels() ?? { mic: 0, out: 0 }
  }

  tap() {
    const p = this.shown.phase
    if (p === 'ringing') return this.answer()
    if (p !== 'live' && p !== 'connecting') return
    const on = !this.shown.muted
    this.set({ muted: on })
    this.run?.line?.setMuted(on)
  }

  end() {
    const p = this.shown.phase
    if (p === 'ringing' && this.call.ring) this.deps.decline(this.call.ring)
    if (p === 'ringing' || p === 'connecting') {
      this.stop()
      return this.leave(null, false)
    }
    if (p === 'hanging') {
      this.stop()
      return this.leave(this.call.conversationId, false)
    }
    if (p !== 'live') return
    this.set({ phase: 'hanging' })
    this.run?.line?.hangUp()
    this.later(() => {
      this.stop()
      this.leave(this.call.conversationId, false)
    }, HANG_UP_WAIT_MS)
  }

  ringTaken(ring: string) {
    if (ring === this.call.ring && this.shown.phase === 'ringing') this.leave(null, false)
  }

  private answer() {
    this.silence()
    this.deps.prime()
    this.set({ phase: 'connecting' })
    void this.begin()
  }

  private async begin() {
    const token: Run = { alive: true, line: null }
    this.run = token
    await this.deps.load()
    if (!token.alive) return
    const line = this.deps.connect({
      state: (live) => {
        const connected = this.shown.phase === 'connecting'
        if (connected) this.deps.chime('connect')
        this.set({ live, phase: connected ? 'live' : this.shown.phase })
      },
      caption: (text) => this.set({ caption: { text, n: ++this.captions } }),
      tools: (running, landed) =>
        this.set({
          working: running > 0,
          ...(landed === null ? {} : { landed: { ok: landed, n: ++this.landings } }),
        }),
      ended: (e) => this.leave(e.conversationId, FAILED.has(e.reason)),
      micDenied: () => {
        this.deps.onMicBlocked()
        this.leave(null, true)
      },
    })
    line.setMuted(this.shown.muted)
    token.line = line
    await line.start()
  }

  private stop() {
    const r = this.run
    if (!r) return
    r.alive = false
    r.line?.close()
  }

  private leave(conversationId: number | null, failed: boolean) {
    if (this.shown.phase === 'gone') return
    this.silence()
    if (this.shown.phase !== 'ringing') this.deps.chime(failed ? 'failed' : 'hangup')
    this.set({ phase: 'gone', shake: failed })
    this.deps.release()
    this.later(() => this.deps.onClose(conversationId), LEAVE_MS)
  }

  private silence() {
    this.ringtone?.()
    this.ringtone = null
  }

  private later(fn: () => void, ms: number) {
    this.timers.push(this.deps.later(fn, ms))
  }

  private set(patch: Partial<CallShown>) {
    this.shown = { ...this.shown, ...patch }
    this.changed(this.shown)
  }
}
