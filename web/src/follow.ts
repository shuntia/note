// Within this many pixels of its end a pane counts as at the end.
export const END_SLACK = 64
// How long after a wheel, touch or key a scroll still counts as the user's own.
export const INTENT_MS = 400

export type Follow = { top: number; intentAt: number; held: boolean }

export const newFollow = (): Follow => ({ top: 0, intentAt: -Infinity, held: false })

export type Geometry = { top: number; height: number; client: number }

/**
 * Whether the pane keeps following its end after a scroll. Content growing under a
 * scroll still easing toward the end never lets go: only a scroll back up that the
 * user is driving does, and reaching the end again always takes hold.
 */
export function follows(f: Follow, stick: boolean, g: Geometry, now: number): boolean {
  const back = g.top < f.top
  f.top = g.top
  if (g.height - g.top - g.client < END_SLACK) return true
  if (back && (f.held || now - f.intentAt < INTENT_MS)) return false
  return stick
}

const SCROLL_KEYS = new Set(['ArrowUp', 'ArrowDown', 'PageUp', 'PageDown', 'Home', 'End', ' '])

export const scrollsByKey = (key: string): boolean => SCROLL_KEYS.has(key)
