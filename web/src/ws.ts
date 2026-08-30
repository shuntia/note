export type EventFrame = {
  type: 'event'
  title: string
  body: string
  urgency: string
  event_id: number | null
}

// Reconnects with capped backoff; the server pings every 30s, so a healthy
// socket stays quiet from our side.
export function connectEvents(onEvent: (ev: EventFrame) => void): () => void {
  let socket: WebSocket | null = null
  let closed = false
  let retry = 1000
  const open = () => {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws'
    socket = new WebSocket(`${proto}://${location.host}/api/ws`)
    socket.onopen = () => {
      retry = 1000
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
      if (!closed) {
        window.setTimeout(open, retry)
        retry = Math.min(retry * 2, 30_000)
      }
    }
  }
  open()
  return () => {
    closed = true
    socket?.close()
  }
}
