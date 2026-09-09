import { useEffect, useRef } from 'react'

export function useEscape(active: boolean, close: () => void) {
  const fn = useRef(close)
  fn.current = close
  useEffect(() => {
    if (!active) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') fn.current()
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [active])
}
