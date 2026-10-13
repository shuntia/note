import { lazy, useState, type ComponentType } from 'react'

export type Chunk<P extends object> = {
  View: ComponentType<P>
  preload: () => Promise<ComponentType<P>>
  ready: () => boolean
}

// A view fetched on first need. Once `preload` has settled the view renders
// straight away; before that it suspends like `React.lazy`. A failed fetch is
// forgotten so the next call tries again.
export function chunk<P extends object>(load: () => Promise<ComponentType<P>>): Chunk<P> {
  let loaded: ComponentType<P> | null = null
  let pending: Promise<ComponentType<P>> | null = null
  const preload = () =>
    (pending ??= load().then(
      (view) => (loaded = view),
      (err: unknown) => {
        pending = null
        throw err
      },
    ))
  const Lazy = lazy(() => preload().then((view) => ({ default: view })))
  function View(props: P) {
    const [Shown] = useState<ComponentType<P>>(() => loaded ?? Lazy)
    return <Shown {...props} />
  }
  return { View, preload, ready: () => loaded !== null }
}

export function whenIdle(fn: () => void): () => void {
  if ('requestIdleCallback' in window) {
    const id = window.requestIdleCallback(fn, { timeout: 2000 })
    return () => window.cancelIdleCallback(id)
  }
  const id = setTimeout(fn, 200)
  return () => clearTimeout(id)
}
