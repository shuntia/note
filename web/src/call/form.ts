export type Look = 'ringing' | 'listening' | 'hearing' | 'thinking' | 'speaking' | 'ending'
export type Form = {
  breath: number
  ripple: number
  pulse: number
  ring: number
  write: number
  dim: number
  gap: number
  scale: number
}
export type Levels = { mic: number; out: number }
export type Pt = { x: number; y: number; a: number }

export const BREATH_MS = 4000
const EASE_TAU_MS = 133
const TWO_PI = Math.PI * 2

const REST: Form = { breath: 0, ripple: 0, pulse: 0, ring: 0, write: 0, dim: 0, gap: 0, scale: 1 }
const LOOKS: Record<Look, Partial<Form>> = {
  ringing: { ring: 1, breath: 0.5 },
  listening: { breath: 1 },
  hearing: { breath: 0.3, ripple: 1 },
  thinking: { write: 1 },
  speaking: { pulse: 1, breath: 0.3 },
  ending: { scale: 0.6, dim: 1 },
}

export function target(look: Look, muted: boolean): Form {
  const f = { ...REST, ...LOOKS[look] }
  return muted && look !== 'ending' && look !== 'ringing' ? { ...f, dim: 0.5, gap: 1 } : f
}

export function stillForm(look: Look, muted: boolean): Form {
  const f = target(look, muted)
  return { ...f, breath: 0, ripple: 0, pulse: 0, ring: 0, write: 0, dim: look === 'thinking' ? Math.max(f.dim, 0.35) : f.dim }
}

export function ease(cur: Form, to: Form, dtMs: number): Form {
  const k = 1 - Math.exp(-dtMs / EASE_TAU_MS)
  const out = { ...cur }
  for (const key of Object.keys(to) as (keyof Form)[]) out[key] = cur[key] + (to[key] - cur[key]) * k
  return out
}

export function smooth(prev: number, raw: number, dtMs: number): number {
  const tau = raw > prev ? 30 : 220
  return prev + (raw - prev) * (1 - Math.exp(-dtMs / tau))
}

// A looping, handwriting-like path through the centre; `a` is how far the nib has travelled.
function scribble(a: number): [number, number] {
  return [0.62 * Math.sin(a) + 0.22 * Math.sin(2.3 * a + 1.1), 0.42 * Math.sin(1.6 * a) + 0.2 * Math.cos(3.7 * a)]
}

const smoothstep = (x: number) => {
  const c = Math.min(1, Math.max(0, x))
  return c * c * (3 - 2 * c)
}

// The whole circle in every state: writing pulls each point from the rim onto the stroke behind the nib,
// nib end first, so the rim unspools into the stroke and curls back the same way.
export function shape(f: Form, tMs: number, lv: Levels, n = 160): Pt[] {
  const breath = 1 + 0.02 * f.breath * Math.sin((TWO_PI * tMs) / BREATH_MS)
  const knock = Math.pow(Math.max(0, Math.sin((TWO_PI * tMs) / 1200)), 4)
  const r0 = f.scale * breath * (1 + 0.08 * f.pulse * lv.out) * (1 + 0.05 * f.ring * knock)
  const turn = tMs / 9000
  const nib = tMs / 650
  const pts: Pt[] = []
  for (let i = 0; i < n; i++) {
    const s = i / (n - 1)
    const th = TWO_PI * s + turn - Math.PI / 2
    const ripple = 0.06 * f.ripple * lv.mic * Math.sin(6 * th + tMs / 90) * (0.6 + 0.4 * Math.sin(3 * th - tMs / 140))
    const r = r0 + ripple
    const [sx, sy] = scribble(nib - (1 - s) * 5.2)
    const w = smoothstep(f.write * 1.6 - (1 - s) * 0.6)
    const ink = 1 - f.write * Math.pow(1 - s, 1.5)
    const fromTop = Math.abs(Math.atan2(Math.sin(th + Math.PI / 2), Math.cos(th + Math.PI / 2)))
    pts.push({
      x: r * Math.cos(th) * (1 - w) + sx * w,
      y: r * Math.sin(th) * (1 - w) + sy * w,
      a: fromTop < 0.06 * f.gap ? 0 : ink,
    })
  }
  return pts
}
