import gsap from 'gsap'

const still = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches

type Target = Element | null | undefined

const live = (els: readonly Target[]): Element[] => els.filter((el): el is Element => !!el)

/** Rows arriving in a list: a short rise, one just behind the next. */
export function settle(targets: readonly Target[], y = 6): void {
  const els = live(targets)
  if (still() || els.length === 0) return
  gsap.from(els, {
    autoAlpha: 0,
    y,
    duration: 0.32,
    ease: 'power2.out',
    stagger: 0.03,
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

/** A row folding away under what follows it; `done` runs at zero height. */
export function collapse(el: Element | null | undefined, done: () => void): void {
  if (still() || !el) return done()
  gsap.to(el, {
    height: 0,
    autoAlpha: 0,
    marginTop: 0,
    marginBottom: 0,
    duration: 0.34,
    ease: 'power2.inOut',
    onComplete: done,
  })
}

/** Rows that changed places: each starts from where it was and slides home. */
export function flip(moves: readonly { el: Element; dy: number }[]): void {
  if (still()) return
  for (const { el, dy } of moves) {
    if (Math.abs(dy) < 1) continue
    gsap.from(el, { y: dy, duration: 0.38, ease: 'power2.out', clearProps: 'transform' })
  }
}
