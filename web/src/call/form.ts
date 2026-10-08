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
  gear: number
}
export type Levels = { mic: number; out: number }
export type Pt = { x: number; y: number; a: number }

export const BREATH_MS = 4000
const EASE_TAU_MS = 133
const TWO_PI = Math.PI * 2

const REST: Form = { breath: 0, ripple: 0, pulse: 0, ring: 0, write: 0, dim: 0, gap: 0, scale: 1, gear: 0 }
const LOOKS: Record<Look, Partial<Form>> = {
  ringing: { ring: 1, breath: 0.5 },
  listening: { breath: 1 },
  hearing: { breath: 0.3, ripple: 1 },
  thinking: { write: 1 },
  speaking: { pulse: 1, breath: 0.3 },
  ending: { scale: 0.6, dim: 1 },
}

// `working` while a tool runs: the rim grows teeth and turns.
export function target(look: Look, muted: boolean, working = false): Form {
  const f = { ...REST, ...LOOKS[look], gear: working && look !== 'ending' ? 1 : 0 }
  return muted && look !== 'ending' && look !== 'ringing' ? { ...f, dim: 0.5, gap: 1 } : f
}

export function stillForm(look: Look, muted: boolean, working = false): Form {
  const f = target(look, muted, working)
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

const GEAR_TEETH = 8
const GEAR_DEPTH = 0.16
const GEAR_TURN_MS = 6000

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
  const turn = (TWO_PI * tMs) / GEAR_TURN_MS
  const pts: Pt[] = []
  for (let i = 0; i < n; i++) {
    const s = i / (n - 1)
    const th = -TWO_PI * (1 - s)
    const ripple =
      0.032 * f.ripple * lv.mic * (0.7 * Math.sin(9 * th - tMs / 160) + 0.3 * Math.sin(11 * th + tMs / 230))
    const tooth = smoothstep(Math.cos(GEAR_TEETH * (th - turn)), -0.2, 0.2)
    const r = r0 + ripple + GEAR_DEPTH * f.gear * (tooth - 0.5)
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

export const MARK_MS = 1100
const MARK_DRAW_MS = 280
const MARK_FADE_FROM_MS = 750
const CHECK = [{ x: -0.36, y: 0.02 }, { x: -0.1, y: 0.28 }, { x: 0.38, y: -0.24 }]
const CROSS = [
  [{ x: -0.27, y: -0.27 }, { x: 0.27, y: 0.27 }],
  [{ x: 0.27, y: -0.27 }, { x: -0.27, y: 0.27 }],
]

export type Mark = { strokes: { x: number; y: number }[][]; alpha: number; dx: number }

// Inks the polyline from its start up to `t` of its length.
function inked(line: { x: number; y: number }[], t: number): { x: number; y: number }[] {
  const lens = line.slice(1).map((p, i) => Math.hypot(p.x - line[i].x, p.y - line[i].y))
  let left = Math.max(0, Math.min(1, t)) * lens.reduce((a, b) => a + b, 0)
  const out = [line[0]]
  for (let i = 0; i < lens.length && left > 0; i++) {
    const k = Math.min(1, left / lens[i])
    out.push({ x: line[i].x + (line[i + 1].x - line[i].x) * k, y: line[i].y + (line[i + 1].y - line[i].y) * k })
    left -= lens[i]
  }
  return out
}

// What a tool that landed draws inside the circle `ageMs` after: a check mark when it worked, an X and a short
// sideways tremor when it failed, each written on, held, then faded. Null once it has played out.
export function mark(ok: boolean, ageMs: number): Mark | null {
  if (!Number.isFinite(ageMs) || ageMs < 0 || ageMs >= MARK_MS) return null
  const drawn = ageMs / MARK_DRAW_MS
  const alpha = 1 - smoothstep(ageMs, MARK_FADE_FROM_MS, MARK_MS)
  if (ok) return { strokes: [inked(CHECK, drawn)], alpha, dx: 0 }
  const shake = Math.min(1, ageMs / 450)
  return {
    strokes: [inked(CROSS[0], drawn * 2), inked(CROSS[1], drawn * 2 - 1)].filter((l) => l.length > 1),
    alpha,
    dx: 0.035 * Math.sin(shake * 3 * TWO_PI) * (1 - shake),
  }
}
