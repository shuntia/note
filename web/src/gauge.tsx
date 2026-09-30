import { gsap } from 'gsap'
import { useLayoutEffect, useRef, type ReactNode, type RefObject } from 'react'
import { C, R as RING, SPAN } from './circle'
import { drain as drainArc } from './fx'
import { reducedMotion } from './motion'

// One drawing at every size: the ring lives in a fixed 320-unit box and the
// wrapper is what grows and shrinks, so the stroke scales with the ring. `size` is
// the width at 1440x900, taken through --u so the arc follows the viewport.
export const VB = 320
export const STROKE = 9
const R = VB / 2 - STROKE * 1.4
const MID = VB / 2
const SEGMENTS = 96
const BREATHE_MS = 7000

// [x0, x1, y] in the ring's own units: the straight line the arc unrolls onto.
export type ArcLine = [number, number, number]

// The 240° arc, open at the bottom, as a path so it can unroll: at t=0 the arc, at
// t=1 the line. pathLength=1 on the element keeps the filled share meaningful.
export function arcPath(t = 0, line?: ArcLine): string {
  let d = ''
  for (let i = 0; i <= SEGMENTS; i++) {
    const u = i / SEGMENTS
    const th = ((150 + 240 * u) * Math.PI) / 180
    let x = MID + R * Math.cos(th)
    let y = MID + R * Math.sin(th)
    if (line) {
      x += (line[0] + u * (line[1] - line[0]) - x) * t
      y += (line[2] - y) * t
    }
    d += `${i ? 'L' : 'M'}${x.toFixed(1)} ${y.toFixed(1)}`
  }
  return d
}

const clamp = (v: number) => Math.min(1, Math.max(0, v))

/**
 * `fracAt` makes the arc live: it is read every frame, and the stroke follows it
 * with no per-second steps. `breathe` is the session arc's slow pulse (both copies of
 * a face read the same clock, so they pulse together); `paused` dims it to half with
 * a tween rather than a cut.
 */
export function Gauge({
  size,
  frac = 0,
  fracAt,
  faded = false,
  breathe = false,
  paused = false,
  children,
}: {
  size: number
  frac?: number
  fracAt?: () => number
  faded?: boolean
  breathe?: boolean
  paused?: boolean
  children?: ReactNode
}) {
  const track = useRef<SVGPathElement>(null)
  const arc = useRef<SVGPathElement>(null)
  const at = useRef(fracAt)
  at.current = fracAt
  const dim = useRef({ v: 0 })

  useLayoutEffect(() => {
    if (reducedMotion()) {
      dim.current.v = paused ? 1 : 0
      return
    }
    const tw = gsap.to(dim.current, { v: paused ? 1 : 0, duration: 0.5, ease: 'power2.out', overwrite: true })
    return () => {
      tw.kill()
    }
  }, [paused])

  useLayoutEffect(() => {
    const path = arc.current
    const rail = track.current
    if (!path || !rail) return
    const still = reducedMotion()
    // The pulse is painted on the strokes, not the svg, so the svg's opacity stays
    // free for the morph to fade it.
    const paint = () => {
      const read = at.current
      if (read) path.setAttribute('stroke-dasharray', `${clamp(read())} 1`)
      if (!breathe) return
      const pulse = still ? 1 : 0.775 + 0.225 * Math.sin((performance.now() / BREATHE_MS) * 2 * Math.PI)
      const opacity = String(pulse + (0.5 - pulse) * dim.current.v)
      path.style.opacity = opacity
      rail.style.opacity = opacity
    }
    paint()
    if (!fracAt && !breathe) return
    let id = requestAnimationFrame(function step() {
      paint()
      id = requestAnimationFrame(step)
    })
    return () => {
      cancelAnimationFrame(id)
      path.style.opacity = ''
      rail.style.opacity = ''
    }
  }, [breathe, !!fracAt])

  const d = arcPath(0)
  return (
    <div className={`gauge${faded ? ' faded' : ''}`} style={{ width: `calc(${size} * var(--u))`, height: `calc(${size} * var(--u))` }}>
      <svg className="gauge-ring" viewBox={`0 0 ${VB} ${VB}`} aria-hidden="true">
        <path ref={track} className="gauge-track" d={d} pathLength={1} strokeWidth={STROKE} />
        <path
          ref={arc}
          className="gauge-arc"
          d={d}
          pathLength={1}
          strokeWidth={STROKE}
          strokeDasharray={fracAt ? undefined : `${clamp(frac)} 1`}
        />
      </svg>
      <div className="gauge-centre">{children}</div>
    </div>
  )
}

const ARC = { cx: 160, cy: 160, r: RING, strokeWidth: STROKE }

/**
 * The session's arc as two circles the fx helpers drive (`track`, `fill`). While
 * `hold` is set the effects own them and the painter keeps off; otherwise the fill
 * follows `fracAt`, or drains from its far end when `drain`.
 */
export function Circle({
  size,
  fracAt,
  drain = false,
  paused = false,
  breathe = false,
  hold,
  fill,
  track,
  children,
}: {
  size: number
  fracAt?: () => number
  drain?: boolean
  paused?: boolean
  breathe?: boolean
  hold: RefObject<boolean>
  fill: RefObject<SVGCircleElement | null>
  track: RefObject<SVGCircleElement | null>
  children?: ReactNode
}) {
  const at = useRef(fracAt)
  at.current = fracAt
  const dim = useRef({ v: paused ? 1 : 0 })

  useLayoutEffect(() => {
    if (reducedMotion()) {
      dim.current.v = paused ? 1 : 0
      return
    }
    const tw = gsap.to(dim.current, { v: paused ? 1 : 0, duration: 0.5, ease: 'power2.out', overwrite: true })
    return () => {
      tw.kill()
    }
  }, [paused])

  useLayoutEffect(() => {
    const f = fill.current
    const t = track.current
    if (!f || !t) return
    const still = reducedMotion()
    const paint = () => {
      if (hold.current) return
      const frac = clamp(at.current?.() ?? 0)
      const pulse = breathe && !still ? 0.775 + 0.225 * Math.sin((performance.now() / BREATHE_MS) * 2 * Math.PI) : 1
      const lit = String(pulse * (1 - 0.62 * dim.current.v))
      if (drain) {
        drainArc(f, SPAN * (1 - frac))
        if (f.style.opacity !== '0') f.style.opacity = lit
        t.style.opacity = String(pulse)
        return
      }
      const len = SPAN * frac
      f.setAttribute('stroke-dasharray', `${len} ${C}`)
      f.setAttribute('stroke-dashoffset', '0')
      f.style.opacity = len > 9 ? lit : '0'
      t.style.opacity = String(pulse)
    }
    paint()
    let id = requestAnimationFrame(function step() {
      paint()
      id = requestAnimationFrame(step)
    })
    return () => cancelAnimationFrame(id)
  }, [drain, breathe, fill, track, hold])

  return (
    <div className="gauge arc" style={{ width: `calc(${size} * var(--u))`, height: `calc(${size} * var(--u))` }}>
      <svg className="gauge-ring" viewBox={`0 0 ${VB} ${VB}`} aria-hidden="true">
        <circle ref={track} className="arc-track" {...ARC} strokeDasharray={`${SPAN} ${C}`} transform="rotate(150 160 160)" />
        <circle ref={fill} className="arc-fill" {...ARC} strokeDasharray={`0 ${C}`} transform="rotate(150 160 160)" />
      </svg>
      <div className="gauge-centre">{children}</div>
    </div>
  )
}
