import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { reducedMotion, STAGE_MS } from './motion'

// One drawing at every size: the ring lives in a fixed 320-unit box and the
// wrapper is what grows and shrinks, so the dash lengths never change with the stage.
const VB = 320
const SWEEP = 240 / 360
const STROKE = 9
const R = VB / 2 - STROKE * 1.4
const C = 2 * Math.PI * R
const FULL = C * SWEEP
const MID = VB / 2

export function Gauge({
  size,
  frac,
  faded = false,
  children,
}: {
  size: number
  frac: number
  faded?: boolean
  children?: ReactNode
}) {
  const prog = FULL * Math.min(1, Math.max(0, frac))
  const box = useRef<HTMLDivElement>(null)
  const last = useRef<{ x: number; y: number; w: number } | null>(null)
  const moving = useRef(0)
  const [live, setLive] = useState(false)

  // The dash tween is for one tick to the next; the first paint shows the reading as is.
  useEffect(() => {
    const id = requestAnimationFrame(() => setLive(true))
    return () => {
      cancelAnimationFrame(id)
      window.clearTimeout(moving.current)
    }
  }, [])

  // Whenever layout has put the ring somewhere else it travels there from where it
  // was (FLIP), so a stage change reads as one shape moving rather than two drawings.
  useLayoutEffect(() => {
    const el = box.current
    if (!el || moving.current) return
    const r = el.getBoundingClientRect()
    const now = { x: r.left + window.scrollX, y: r.top + window.scrollY, w: r.width }
    const prev = last.current
    last.current = now
    if (!prev || reducedMotion() || !now.w || !prev.w) return
    const dx = prev.x - now.x
    const dy = prev.y - now.y
    const s = prev.w / now.w
    if (dx === 0 && dy === 0 && s === 1) return
    el.style.transformOrigin = '0 0'
    el.style.transition = 'none'
    el.style.transform = `translate(${dx}px, ${dy}px) scale(${s})`
    void el.offsetWidth
    el.style.transition = `transform ${STAGE_MS}ms cubic-bezier(.2,.7,.3,1)`
    el.style.transform = ''
    moving.current = window.setTimeout(() => {
      moving.current = 0
      el.style.transition = ''
      el.style.transform = ''
      el.style.transformOrigin = ''
      const b = el.getBoundingClientRect()
      last.current = { x: b.left + window.scrollX, y: b.top + window.scrollY, w: b.width }
    }, STAGE_MS)
  })

  return (
    <div ref={box} className={`gauge${faded ? ' faded' : ''}`} style={{ width: size, height: size }}>
      <svg className="gauge-ring" viewBox={`0 0 ${VB} ${VB}`} aria-hidden="true">
        <g transform={`rotate(150 ${MID} ${MID})`}>
          <circle className="gauge-track" cx={MID} cy={MID} r={R} strokeWidth={STROKE} strokeDasharray={`${FULL} ${C}`} />
          {prog > 0 && (
            <circle
              className={`gauge-arc${live ? ' live' : ''}`}
              cx={MID}
              cy={MID}
              r={R}
              strokeWidth={STROKE}
              strokeDasharray={`${prog} ${C}`}
            />
          )}
        </g>
      </svg>
      <div className="gauge-centre">{children}</div>
    </div>
  )
}
