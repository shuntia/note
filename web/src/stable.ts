import { useCallback, useMemo, useRef } from 'react'

// Keeps one identity for the component's life and always calls the latest `fn`,
// so it is safe to call during render as well as from handlers.
export function useStableCallback<A extends unknown[], R>(fn: (...args: A) => R): (...args: A) => R {
  const ref = useRef(fn)
  ref.current = fn
  return useCallback((...args: A) => ref.current(...args), [])
}

// One object for the component's life whose methods call the latest given ones;
// the set of keys must not change between renders.
export function useStableMethods<T extends Record<string, (...args: any[]) => unknown>>(methods: T): T {
  const ref = useRef(methods)
  ref.current = methods
  return useMemo(
    () =>
      Object.fromEntries(
        Object.keys(ref.current).map((key) => [key, (...args: unknown[]) => ref.current[key](...args)]),
      ) as T,
    [],
  )
}

const plain = (x: unknown): x is Record<string, unknown> =>
  typeof x === 'object' && x !== null && Object.getPrototypeOf(x) === Object.prototype

const idOf = (x: unknown) => (plain(x) && 'id' in x ? x.id : undefined)

// `next` with each part equal to its counterpart in `prev` swapped for that one, so a refetch
// that changed nothing keeps every identity and one that changed a row renews only that row
// and what holds it. Array items with an `id` are matched by it, others by index.
export function reuse<T>(prev: T, next: T): T {
  if (Object.is(prev, next)) return prev
  if (Array.isArray(prev) && Array.isArray(next)) {
    const byId = new Map(prev.map((p) => [idOf(p), p]))
    const out = next.map((x, i) => {
      const id = idOf(x)
      return reuse(id === undefined ? prev[i] : byId.get(id), x)
    })
    return (out.length === prev.length && out.every((x, i) => x === prev[i]) ? prev : out) as T
  }
  if (plain(prev) && plain(next)) {
    const keys = Object.keys(next)
    const out: Record<string, unknown> = {}
    for (const k of keys) out[k] = reuse(prev[k], next[k])
    const same = keys.length === Object.keys(prev).length && keys.every((k) => k in prev && out[k] === prev[k])
    return (same ? prev : out) as T
  }
  return next
}

// The previous array while the next one holds the same items in the same order.
export function useSameArray<T>(next: T[]): T[] {
  const ref = useRef(next)
  const prev = ref.current
  if (prev.length !== next.length || prev.some((item, i) => item !== next[i])) ref.current = next
  return ref.current
}
