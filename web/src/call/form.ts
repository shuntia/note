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
  if (!Number.isFinite(dtMs) || dtMs <= 0) return cur
  const k = 1 - Math.exp(-dtMs / EASE_TAU_MS)
  const out = { ...cur }
  for (const key of Object.keys(to) as (keyof Form)[]) out[key] = cur[key] + (to[key] - cur[key]) * k
  return out
}

// A non-finite level counts as silence.
export function smooth(prev: number, raw: number, dtMs: number): number {
  if (!Number.isFinite(dtMs) || dtMs <= 0) return prev
  if (!Number.isFinite(raw)) raw = 0
  const tau = raw > prev ? 30 : 220
  return prev + (raw - prev) * (1 - Math.exp(-dtMs / tau))
}

// Slanted cursive loops (a prolate trochoid): advance per radian, loop half-width and half-height, slant,
// nib speed, and how many loops stay inked.
const LOOP_ADVANCE = 0.085
const LOOP_W = 0.27
const LOOP_H = 0.26
const SLANT = 0.2
const NIB_RAD_PER_MS = TWO_PI / 1100
const INKED_RAD = 2.6 * TWO_PI
const NIB_X = (LOOP_ADVANCE * INKED_RAD) / 2

const smoothstep = (x: number, lo = 0, hi = 1) => {
  const c = Math.min(1, Math.max(0, (x - lo) / (hi - lo)))
  return c * c * (3 - 2 * c)
}

// The whole circle in every state. The rim runs from its right-hand point all the way round, nib end last;
// writing peels each point off the rim onto the cursive line, nib end first, so the rim opens at the right and
// unrolls into the line, and curls back the same way. The nib loops in place while the line slides left under it.
export function shape(f: Form, tMs: number, lv: Levels, n = 160): Pt[] {
  const breath = 1 + 0.02 * f.breath * Math.sin((TWO_PI * tMs) / BREATH_MS)
  const knock = Math.pow(Math.max(0, Math.sin((TWO_PI * tMs) / 1200)), 4)
  const r0 = f.scale * breath * (1 + 0.08 * f.pulse * lv.out) * (1 + 0.05 * f.ring * knock)
  const nib = tMs * NIB_RAD_PER_MS
  const baseline = 0.04 * Math.sin((TWO_PI * tMs) / 7000)
  const pts: Pt[] = []
  for (let i = 0; i < n; i++) {
    const s = i / (n - 1)
    const th = -TWO_PI * (1 - s)
    const ripple =
      0.032 * f.ripple * lv.mic * (0.7 * Math.sin(9 * th - tMs / 160) + 0.3 * Math.sin(11 * th + tMs / 230))
    const r = r0 + ripple
    const behind = (1 - s) * INKED_RAD
    const u = nib - behind
    const sy = -LOOP_H * Math.cos(u)
    const sx = NIB_X - LOOP_ADVANCE * behind - LOOP_W * Math.sin(u) - SLANT * sy
    const w = smoothstep(f.write * 1.3 - (1 - s) * 0.3)
    const fromTop = Math.abs(Math.atan2(Math.sin(th + Math.PI / 2), Math.cos(th + Math.PI / 2)))
    const open = 1 - f.gap * (1 - w) * (1 - smoothstep(fromTop, 0.07, 0.19))
    pts.push({
      x: r * Math.cos(th) * (1 - w) + sx * w,
      y: r * Math.sin(th) * (1 - w) + (sy + baseline) * w,
      a: ((1 - w) + w * smoothstep(s, 0, 0.55)) * open,
    })
  }
  return pts
}

export type Blip = { echo: number; echoAlpha: number; dx: number }
export const BLIP_DONE_MS = 700
export const BLIP_FAILED_MS = 450

// A tool that landed, `ageMs` after: a faint ring swells out when it worked, the circle trembles sideways when it
// failed. Null once it has played out.
export function blip(ok: boolean, ageMs: number): Blip | null {
  if (!Number.isFinite(ageMs) || ageMs < 0) return null
  if (ok) {
    const p = ageMs / BLIP_DONE_MS
    if (p >= 1) return null
    return { echo: 1 + 0.3 * (1 - (1 - p) ** 3), echoAlpha: 0.5 * (1 - p) ** 2, dx: 0 }
  }
  const p = ageMs / BLIP_FAILED_MS
  if (p >= 1) return null
  return { echo: 0, echoAlpha: 0, dx: 0.035 * Math.sin(p * 3 * TWO_PI) * (1 - p) }
}
