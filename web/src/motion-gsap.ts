import gsap from 'gsap'
import { waveDelays } from './wave'

const still = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches

type Target = Element | null | undefined

const live = (els: readonly Target[]): Element[] => els.filter((el): el is Element => !!el)

/** Rows arriving in a list: one wave from the top of `within` (the viewport by
 *  default), a quarter second long whatever the count; rows outside it do not move. */
export function settle(targets: readonly Target[], y = 6, within?: Element): void {
  const els = live(targets)
  if (still() || els.length === 0) return
  const box = within
    ? within.getBoundingClientRect()
    : new DOMRect(0, 0, window.innerWidth, window.innerHeight)
  const delays = waveDelays(els.map((el) => el.getBoundingClientRect()), box)
  const shown: Element[] = []
  const delay: number[] = []
  delays.forEach((d, i) => {
    if (d === null) return
    shown.push(els[i])
    delay.push(d)
  })
  if (shown.length === 0) return
  gsap.from(shown, {
    autoAlpha: 0,
    y,
    duration: 0.32,
    ease: 'power2.out',
    stagger: (i: number) => delay[i],
    overwrite: 'auto',
    clearProps: 'transform,opacity,visibility',
  })
}

/** A menu or card opening from its trigger; `up` for one that opens above it. */
export function popIn(el: Target, up = false): void {
  if (still() || !el) return
  gsap.from(el, {
    autoAlpha: 0,
    y: up ? 6 : -6,
    scale: 0.97,
    transformOrigin: up ? 'bottom right' : 'top right',
    duration: 0.24,
    ease: 'power2.out',
    clearProps: 'transform,opacity,visibility',
  })
}

/** The reverse of `popIn`; `done` runs once the element is free to unmount. */
export function popOut(el: Target, done: () => void, up = false): void {
  if (still() || !el) return done()
  gsap.to(el, {
    autoAlpha: 0,
    y: up ? 6 : -6,
    scale: 0.97,
    transformOrigin: up ? 'bottom right' : 'top right',
    duration: 0.18,
    ease: 'power2.in',
    onComplete: done,
  })
}

/** A panel or fact taking its place: further and slower than a menu's pop. */
export function rise(el: Target, y = 20): void {
  if (still() || !el) return
  gsap.from(el, {
    autoAlpha: 0,
    y,
    duration: 0.4,
    ease: 'expo.out',
    clearProps: 'transform,opacity,visibility',
  })
}

/** A row folding away under what follows it, after `delay` seconds; `done` runs
 *  at zero height. The returned kill stops the fold and puts the row back. */
export function collapse(el: Element | null | undefined, done: () => void, delay = 0): () => void {
  if (still() || !el) {
    done()
    return () => {}
  }
  const tween = gsap.to(el, {
    height: 0,
    autoAlpha: 0,
    marginTop: 0,
    marginBottom: 0,
    duration: 0.34,
    delay,
    ease: 'power2.inOut',
    onComplete: done,
  })
  return () => {
    if (tween.progress() === 1) return
    tween.kill()
    gsap.set(el, { clearProps: 'height,opacity,visibility,marginTop,marginBottom' })
  }
}

/** A panel growing out of the box above it, from nothing to the height it asks for. */
export function unfold(el: Target): void {
  if (still() || !el) return
  gsap.from(el, {
    height: 0,
    paddingTop: 0,
    paddingBottom: 0,
    autoAlpha: 0,
    duration: 0.28,
    ease: 'expo.out',
    clearProps: 'height,padding,opacity,visibility',
  })
}

/** The reverse of `unfold`; `done` runs once the panel is free to unmount. */
export function fold(el: Target, done: () => void): void {
  if (still() || !el) return done()
  gsap.to(el, {
    height: 0,
    paddingTop: 0,
    paddingBottom: 0,
    autoAlpha: 0,
    duration: 0.2,
    ease: 'power2.in',
    onComplete: done,
  })
}

/** Rows that changed places: each starts from where it was and slides home. */
export function flip(moves: readonly { el: Element; dx?: number; dy: number }[]): void {
  if (still()) return
  for (const { el, dx = 0, dy } of moves) {
    if (Math.abs(dx) < 1 && Math.abs(dy) < 1) continue
    gsap.from(el, { x: dx, y: dy, duration: 0.38, ease: 'power2.out', overwrite: 'auto', clearProps: 'transform' })
  }
}

/** The view giving way; `done` runs once it is free to unmount. */
export function viewOut(el: Target, done: () => void): void {
  if (still() || !el) return done()
  gsap.to(el, {
    autoAlpha: 0,
    y: -8,
    duration: 0.18,
    ease: 'power2.in',
    overwrite: true,
    onComplete: done,
  })
}

/** The view taking the screen, rising into the place the last one held. */
export function viewIn(el: Target, done: () => void): void {
  if (still() || !el) return done()
  gsap.fromTo(
    el,
    { opacity: 0, y: 8 },
    {
      opacity: 1,
      y: 0,
      duration: 0.28,
      delay: 0.06,
      ease: 'power2.out',
      overwrite: true,
      onComplete: () => {
        // A pin inside the arriving view needs its layer free of transforms.
        gsap.set(el, { clearProps: 'transform,opacity' })
        done()
      },
    },
  )
}

/** Holds a view at the pixels it occupies so the next one can take the layout under it. */
export function lift(el: HTMLElement): void {
  const y = Number(gsap.getProperty(el, 'y')) || 0
  const r = el.getBoundingClientRect()
  Object.assign(el.style, {
    position: 'fixed',
    left: `${r.left}px`,
    top: `${r.top - y}px`,
    width: `${r.width}px`,
    height: `${r.height}px`,
  })
}

/** The mark behind the current view's name, sliding to the one just chosen. */
export function glide(el: Target, to: { x: number; width: number }, animate: boolean): void {
  if (!el) return
  if (animate && !still()) gsap.to(el, { ...to, duration: 0.34, ease: 'power3.out', overwrite: true })
  else gsap.set(el, to)
}
