import { useSyncExternalStore } from 'react'

export type MotionChoice = 'system' | 'reduced' | 'full'

const REDUCED_MOTION = '(prefers-reduced-motion: reduce)'

let choice: MotionChoice = 'system'
const listeners = new Set<() => void>()
let media: MediaQueryList | null = null

export const reducedMotion = () =>
  choice === 'reduced' || (choice === 'system' && window.matchMedia(REDUCED_MOTION).matches)

// The root carries data-motion="reduced" whenever motion is held still, which is what the
// stylesheets key on in place of the media query; index.html sets it before first paint.
function paint() {
  const root = document.documentElement
  if (reducedMotion()) root.setAttribute('data-motion', 'reduced')
  else root.removeAttribute('data-motion')
  for (const l of listeners) l()
}

export function applyMotion(next: MotionChoice) {
  choice = next
  if (!media) {
    media = window.matchMedia(REDUCED_MOTION)
    media.addEventListener('change', () => choice === 'system' && paint())
  }
  paint()
}

function subscribe(l: () => void) {
  listeners.add(l)
  return () => void listeners.delete(l)
}

export const useReducedMotion = () => useSyncExternalStore(subscribe, reducedMotion)
