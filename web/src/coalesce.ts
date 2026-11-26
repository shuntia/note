/** One call, `ms` after the last of a burst. */
export function trailing(fn: () => void, ms: number): () => void {
  let timer: ReturnType<typeof setTimeout> | undefined
  return () => {
    if (timer !== undefined) clearTimeout(timer)
    timer = setTimeout(() => {
      timer = undefined
      fn()
    }, ms)
  }
}

/** Only the newest request's answer or failure counts; an older one resolves to undefined. */
export function latest<T>(): (p: Promise<T>) => Promise<T | undefined> {
  let seq = 0
  return (p) => {
    const mine = ++seq
    return p.then(
      (v) => (mine === seq ? v : undefined),
      (err: unknown) => (mine === seq ? Promise.reject(err) : undefined),
    )
  }
}

export type Quiet = { poke: () => void; hold: (on: boolean) => void; cancel: () => void }

/** One call once the pokes have paused for `ms`; while held it waits for the release, though never past `cap` after the last poke. */
export function quiet(fn: () => void, ms: number, cap: number): Quiet {
  let timer: ReturnType<typeof setTimeout> | undefined
  let last = 0
  let held = false
  let due = false
  const fire = () => {
    clearTimeout(timer)
    timer = undefined
    due = false
    fn()
  }
  const check = () => {
    const since = Date.now() - last
    if (held && since < cap) {
      due = true
      timer = setTimeout(check, cap - since)
    } else fire()
  }
  return {
    poke: () => {
      last = Date.now()
      due = false
      clearTimeout(timer)
      timer = setTimeout(check, ms)
    },
    hold: (on) => {
      held = on
      if (!on && due) fire()
    },
    cancel: () => {
      clearTimeout(timer)
      timer = undefined
      due = false
    },
  }
}
