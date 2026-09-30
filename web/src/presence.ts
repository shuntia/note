export const PRESENCE_EVERY_MS = 60_000

const INTERACTIONS = ['pointerdown', 'keydown', 'wheel', 'touchstart'] as const

export type PresenceDeps = {
  ping: () => void
  target: EventTarget
  visible: () => boolean
  now: () => number
}

// Pings at a touch of a visible page, then at most once a minute while touches keep coming.
export function startPresence({ ping, target, visible, now }: PresenceDeps): () => void {
  let last = -Infinity
  let timer: ReturnType<typeof setTimeout> | undefined
  const send = () => {
    last = now()
    ping()
  }
  const onTouch = () => {
    if (!visible() || timer !== undefined) return
    const wait = last + PRESENCE_EVERY_MS - now()
    if (wait <= 0) send()
    else
      timer = setTimeout(() => {
        timer = undefined
        if (visible()) send()
      }, wait)
  }
  for (const type of INTERACTIONS) target.addEventListener(type, onTouch, { passive: true })
  return () => {
    clearTimeout(timer)
    timer = undefined
    for (const type of INTERACTIONS) target.removeEventListener(type, onTouch)
  }
}
