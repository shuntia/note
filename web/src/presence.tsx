import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { reducedMotion, STAGE_MS } from './motion'

type Spot = { top: number; left: number; width: number }

// A block that fades in when shown and, when hidden, fades out where it stood:
// the leaving copy is lifted out of the flow so what replaces it lands at once.
export function Presence({
  show,
  className,
  children,
}: {
  show: boolean
  className: string
  children: ReactNode
}) {
  const [gone, setGone] = useState(!show)
  const el = useRef<HTMLDivElement>(null)
  const spot = useRef<Spot | null>(null)
  const kept = useRef(children)
  if (show) kept.current = children

  useLayoutEffect(() => {
    const node = el.current
    if (!show || !node) return
    spot.current = { top: node.offsetTop, left: node.offsetLeft, width: node.offsetWidth }
  })

  useEffect(() => {
    if (show) {
      setGone(false)
      return
    }
    const id = window.setTimeout(() => setGone(true), reducedMotion() ? 0 : STAGE_MS)
    return () => window.clearTimeout(id)
  }, [show])

  if (!show && (gone || reducedMotion())) return null
  return (
    <div
      ref={el}
      className={show ? className : `${className} leaving`}
      style={show ? undefined : { position: 'absolute', ...spot.current }}
    >
      {show ? children : kept.current}
    </div>
  )
}
