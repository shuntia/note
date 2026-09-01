import { useEffect, useRef } from 'react'
import type { CounterMode } from './session'

function fmt(totalSeconds: number): string {
  const m = Math.floor(totalSeconds / 60)
  const s = totalSeconds % 60
  return `${m}:${String(s).padStart(2, '0')}`
}

function makeCell(c: string): HTMLSpanElement {
  const cell = document.createElement('span')
  cell.className = c === ':' ? 'cell colon' : 'cell'
  const glyph = document.createElement('span')
  glyph.className = 'glyph'
  glyph.textContent = c
  cell.append(glyph)
  return cell
}

// A cell whose character is unchanged is not touched at all, so its running
// animation and its DOM node both survive the tick.
function render(el: HTMLElement, text: string, animate: boolean): void {
  const cells = el.children
  if (cells.length !== text.length) {
    el.replaceChildren(...[...text].map(makeCell))
    return
  }
  ;[...text].forEach((c, i) => {
    const cell = cells[i] as HTMLElement
    const glyph = cell.querySelector('.glyph') as HTMLElement
    if (glyph.textContent === c) return
    if (animate) {
      cell.querySelector('.ghost')?.remove()
      const ghost = document.createElement('span')
      ghost.className = 'ghost'
      ghost.textContent = glyph.textContent ?? ''
      ghost.addEventListener('animationend', () => ghost.remove())
      cell.append(ghost)
    }
    glyph.textContent = c
    if (animate) {
      glyph.classList.remove('chg')
      void glyph.offsetWidth
      glyph.classList.add('chg')
    }
  })
}

/**
 * React renders the container once and never the digits: re-rendering would
 * replace text nodes and kill animations mid-flight. `pausedAt` freezes the
 * clock being read; `startedAt` already carries accumulated pause, so resuming
 * continues from the frozen value.
 */
export function NowCounter({
  startedAt,
  durationSec,
  mode,
  pausedAt,
}: {
  startedAt: number
  durationSec: number
  mode: CounterMode
  pausedAt: number | null
}) {
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const el = ref.current
    if (!el) return
    const value = () => {
      const elapsed = Math.max(0, Math.floor(((pausedAt ?? Date.now()) - startedAt) / 1000))
      return mode === 'remaining' ? Math.max(0, durationSec - elapsed) : elapsed
    }
    let last = -1
    const tick = (animate: boolean) => {
      const v = value()
      if (v === last) return
      last = v
      render(el, fmt(v), animate)
    }
    // The poll runs faster than the second it paints, so the displayed second
    // stays pinned to the real clock instead of accumulating interval drift.
    tick(false)
    const id = setInterval(() => tick(!document.hidden), 250)
    const onVisible = () => {
      if (!document.hidden) tick(false)
    }
    document.addEventListener('visibilitychange', onVisible)
    return () => {
      clearInterval(id)
      document.removeEventListener('visibilitychange', onVisible)
    }
  }, [startedAt, durationSec, mode, pausedAt])

  // key forces a clean rebuild when mode flips, so the flip never animates every digit
  return <div className="now-counter" aria-live="off" key={mode} ref={ref} />
}
