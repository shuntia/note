import type { ReactNode } from 'react'

// 240° of the ring, open at the bottom; the sweep starts at the lower-left end.
const SWEEP = 240 / 360

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
  const stroke = Math.max(4, Math.round(size * 0.028))
  const r = size / 2 - stroke * 1.4
  const c = 2 * Math.PI * r
  const full = c * SWEEP
  const prog = full * Math.min(1, Math.max(0, frac))
  const mid = size / 2
  return (
    <div className={`gauge${faded ? ' faded' : ''}`} style={{ width: size, height: size }}>
      <svg className="gauge-ring" viewBox={`0 0 ${size} ${size}`} aria-hidden="true">
        <g transform={`rotate(150 ${mid} ${mid})`}>
          <circle className="gauge-track" cx={mid} cy={mid} r={r} strokeWidth={stroke} strokeDasharray={`${full} ${c}`} />
          {prog > 0 && (
            <circle className="gauge-arc" cx={mid} cy={mid} r={r} strokeWidth={stroke} strokeDasharray={`${prog} ${c}`} />
          )}
        </g>
      </svg>
      <div className="gauge-centre">{children}</div>
    </div>
  )
}
