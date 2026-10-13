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

// The previous array while the next one holds the same items in the same order.
export function useSameArray<T>(next: T[]): T[] {
  const ref = useRef(next)
  const prev = ref.current
  if (prev.length !== next.length || prev.some((item, i) => item !== next[i])) ref.current = next
  return ref.current
}
