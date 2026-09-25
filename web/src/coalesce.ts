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
