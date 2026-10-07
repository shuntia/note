import { api } from './api'
import type { FailureReason } from './types'

// `conversation_id` is the thread a check-in opened; null on every other event.
export type EventFrame = {
  type: 'event'
  title: string
  body: string
  urgency: string
  event_id: number | null
  conversation_id: number | null
}

export type AgentEvent =
  | { kind: 'thinking'; text: string }
  | { kind: 'tool_call'; index: number; name: string; args: string }
  | { kind: 'tool_result'; index: number; name: string; result: string; is_error: boolean }
  | { kind: 'reply'; text: string }
  | { kind: 'error'; reason: FailureReason }

// `seq` counts from zero within one session; `conversation_id` is null until a
// brand-new conversation's reply hands the client its id.
export type AgentFrame = {
  type: 'agent'
  conversation_id: number | null
  seq: number
  event: AgentEvent
}

const agentListeners = new Set<(frame: AgentFrame) => void>()

// Subscribes to the live agent frames of the one socket the shell already holds.
export function onAgentFrame(fn: (frame: AgentFrame) => void): () => void {
  agentListeners.add(fn)
  return () => {
    agentListeners.delete(fn)
  }
}

// Handshakes that close without ever opening, this many in a row, read as a
// dead session rather than a flaky network.
const DEAD_HANDSHAKE_STREAK = 3

// Reconnects with capped backoff; the server pings every 30s, so a healthy
// socket stays quiet from our side. A `changed` frame carries no detail: it
// says only that something the client is showing was decided elsewhere.
export function connectEvents(
  onEvent: (ev: EventFrame) => void,
  onChanged: () => void,
): () => void {
  let socket: WebSocket | null = null
  let closed = false
  let retry = 1000
  let timer = 0
  let deadHandshakes = 0
  let sessionChecked = false
  const open = () => {
    if (closed) return
    const proto = location.protocol === 'https:' ? 'wss' : 'ws'
    socket = new WebSocket(`${proto}://${location.host}/api/ws`)
    let opened = false
    socket.onopen = () => {
      opened = true
      retry = 1000
      deadHandshakes = 0
      sessionChecked = false
    }
    socket.onmessage = (m) => {
      try {
        const frame: unknown = JSON.parse(String(m.data))
        if (typeof frame !== 'object' || frame === null) return
        const kind = (frame as { type?: unknown }).type
        if (kind === 'event') onEvent(frame as EventFrame)
        if (kind === 'changed') onChanged()
        if (kind === 'agent') for (const fn of agentListeners) fn(frame as AgentFrame)
      } catch {
        // non-JSON frame; ignore
      }
    }
    socket.onclose = () => {
      if (closed) return
      if (!opened) deadHandshakes += 1
      if (deadHandshakes >= DEAD_HANDSHAKE_STREAK && !sessionChecked) {
        // api.me's 401 path drops the shell back to the login screen.
        sessionChecked = true
        void api.me().catch(() => {})
      }
      timer = window.setTimeout(open, retry)
      retry = Math.min(retry * 2, 30_000)
    }
  }
  open()
  return () => {
    closed = true
    window.clearTimeout(timer)
    socket?.close()
  }
}
