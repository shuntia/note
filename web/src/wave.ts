/** One delay per rect, from 0 for the highest top to `total` for the lowest; null for a
 *  rect outside `within`. */
export function waveDelays(
  rects: readonly DOMRectReadOnly[],
  within?: DOMRectReadOnly,
  total = 0.24,
): (number | null)[] {
  const seen = rects.map((r) => !within || (r.bottom > within.top && r.top < within.bottom))
  const tops = rects.filter((_, i) => seen[i]).map((r) => r.top)
  if (tops.length === 0) return rects.map(() => null)
  const lo = Math.min(...tops)
  const hi = Math.max(...tops)
  return rects.map((r, i) => (seen[i] ? (hi === lo ? 0 : ((r.top - lo) / (hi - lo)) * total) : null))
}
