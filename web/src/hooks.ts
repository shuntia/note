import { useEffect, useState, type DependencyList } from 'react'
import { latest } from './coalesce'

export type Loaded<T> = T | 'error' | undefined

type LoadOptions = {
  blank?: boolean
  skip?: boolean
  expire?: (err: unknown) => boolean
}

/**
 * Loads `fn` on mount and whenever `deps` change, unless `skip`. `blank` clears the old
 * value while a load runs; a failure `expire` claims leaves the value as it was.
 * Returns the value, a reload, and a setter for local edits.
 */
export function useLoad<T>(fn: () => Promise<T>, deps: DependencyList, opts: LoadOptions = {}) {
  const [value, setValue] = useState<Loaded<T>>(undefined)
  const [newest] = useState(() => latest<T>())

  const load = () => {
    if (opts.blank) setValue(undefined)
    newest(fn()).then(
      (v) => {
        if (v !== undefined) setValue(v)
      },
      (err: unknown) => {
        if (!opts.expire?.(err)) setValue('error')
      },
    )
  }

  useEffect(() => {
    if (!opts.skip) load()
  }, deps)

  return [value, load, setValue] as const
}

export type Save = { row?: string; kind: 'busy' | 'saved' | 'failed'; message?: string } | null

/**
 * `run` marks `row` busy while `work` runs, then saved with the message `work` resolves to
 * (null clears the status), or failed with `fail`'s message (null leaves the status as it is).
 */
export function useSave() {
  const [save, setSave] = useState<Save>(null)

  const run = async (
    work: () => Promise<string | null | void>,
    fail: (err: unknown) => string | null,
    row?: string,
  ) => {
    setSave({ row, kind: 'busy' })
    try {
      const message = await work()
      setSave(message === null ? null : { row, kind: 'saved', message: message ?? undefined })
      return true
    } catch (err) {
      const message = fail(err)
      if (message !== null) setSave({ row, kind: 'failed', message })
      return false
    }
  }

  return { save, setSave, busy: save?.kind === 'busy', run }
}
