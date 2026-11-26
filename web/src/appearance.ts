import { applyMotion, type MotionChoice } from './motion'

export type FontChoice = 'atkinson' | 'system' | 'serif' | 'mono'
export type TextSize = 'small' | 'normal' | 'large' | 'larger'
export type Density = 'comfortable' | 'compact'
export type Appearance = { motion: MotionChoice; font: FontChoice; text: TextSize; density: Density }

export const MOTIONS: readonly MotionChoice[] = ['system', 'reduced', 'full']
export const FONTS: readonly FontChoice[] = ['atkinson', 'system', 'serif', 'mono']
export const TEXT_SIZES: readonly TextSize[] = ['small', 'normal', 'large', 'larger']
export const DENSITIES: readonly Density[] = ['comfortable', 'compact']

// index.html reads this key in its pre-paint script; keep the two in step.
const KEY = 'note.appearance'

export const DEFAULT_APPEARANCE: Appearance = { motion: 'system', font: 'atkinson', text: 'normal', density: 'comfortable' }

const pick = <T>(all: readonly T[], v: unknown, fallback: T): T => (all.includes(v as T) ? (v as T) : fallback)

export function storedAppearance(): Appearance {
  try {
    const p = JSON.parse(localStorage.getItem(KEY) ?? 'null') as Partial<Appearance> | null
    if (!p || typeof p !== 'object') return DEFAULT_APPEARANCE
    const d = DEFAULT_APPEARANCE
    return {
      motion: pick(MOTIONS, p.motion, d.motion),
      font: pick(FONTS, p.font, d.font),
      text: pick(TEXT_SIZES, p.text, d.text),
      density: pick(DENSITIES, p.density, d.density),
    }
  } catch {
    return DEFAULT_APPEARANCE
  }
}

export function saveAppearance(a: Appearance) {
  try {
    localStorage.setItem(KEY, JSON.stringify(a))
  } catch {
    // the choice still holds for this session
  }
}

// A default leaves its attribute off, so the stylesheet's own values stand.
export function applyAppearance(a: Appearance) {
  const root = document.documentElement
  const mark = (name: string, v: string, base: string) =>
    v === base ? root.removeAttribute(name) : root.setAttribute(name, v)
  mark('data-font', a.font, DEFAULT_APPEARANCE.font)
  mark('data-text', a.text, DEFAULT_APPEARANCE.text)
  mark('data-density', a.density, DEFAULT_APPEARANCE.density)
  applyMotion(a.motion)
}
