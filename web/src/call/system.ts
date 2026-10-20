import { desktop } from '../desktop'

type CallKeys = { end(): void; toggleMic(): void }
type CallSession = MediaSession & { setMicrophoneActive?(active: boolean): void }

const session = (): CallSession | undefined =>
  typeof navigator !== 'undefined' && 'mediaSession' in navigator ? (navigator.mediaSession as CallSession) : undefined

function handle(action: string, fn: (() => void) | null) {
  try {
    session()?.setActionHandler(action as MediaSessionAction, fn)
  } catch {}
}

// The OS's side of an open call: the desktop app pauses other players and keeps the
// screen awake, and the system media controls hang up and mute where they can.
export function systemCall(keys: CallKeys): () => void {
  desktop()?.inCall?.(true)
  handle('hangup', () => keys.end())
  handle('togglemicrophone', () => keys.toggleMic())
  return () => {
    desktop()?.inCall?.(false)
    handle('hangup', null)
    handle('togglemicrophone', null)
  }
}

export function systemMuted(muted: boolean): void {
  try {
    session()?.setMicrophoneActive?.(!muted)
  } catch {}
}
