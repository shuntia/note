const UNDO_MS = 5000

export type Hold<K> = {
  held: () => K | null
  start: (key: K, send: () => void) => void
  cancel: (key: K) => boolean
}

// A write with no server-side reversal waits out its undo window here. The hold
// sits outside React so it outlives the component that started it, and page hide
// is its last chance to go out.
export function makeHold<K>(windowMs: number = UNDO_MS): Hold<K> {
  let held: { key: K; timer: number; send: () => void } | null = null

  const commit = () => {
    if (!held) return
    const { timer, send } = held
    held = null
    window.clearTimeout(timer)
    send()
  }
  window.addEventListener('pagehide', commit)

  return {
    held: () => (held ? held.key : null),
    start: (key, send) => {
      commit()
      held = { key, timer: window.setTimeout(commit, windowMs), send }
    },
    cancel: (key) => {
      if (!held || held.key !== key) return false
      window.clearTimeout(held.timer)
      held = null
      return true
    },
  }
}
