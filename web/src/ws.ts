import { api } from './api'

export type EventFrame = {
  type: 'event'
  title: string
  body: string
  urgency: string
  event_id: number | null
}

// Handshakes that close without ever opening, this many in a row, read as a
// dead session rather than a flaky network.
const DEAD_HANDSHAKE_STREAK = 3

// Reconnects with capped backoff; the server pings every 30s, so a healthy
// socket stays quiet from our side.
export function connectEvents(onEvent: (ev: EventFrame) => void): () => void {
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
        if (
          typeof frame === 'object' &&
          frame !== null &&
          (frame as { type?: unknown }).type === 'event'
        ) {
          onEvent(frame as EventFrame)
        }
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
