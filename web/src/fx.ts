import gsap from 'gsap'
import { beadAt, C, SPAN } from './circle'

// Every effect takes the face (the 320 box holding `.arc svg`); fill and track are its two circles.

const NS = 'http://www.w3.org/2000/svg'
const still = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches
const svgOf = (face: Element) => face.querySelector<SVGSVGElement>('.arc svg')
const homeOf = (face: Element) => face.closest<HTMLElement>('.home')

function circle(attrs: Record<string, string | number>): SVGCircleElement {
  const c = document.createElementNS(NS, 'circle')
  for (const [k, v] of Object.entries(attrs)) c.setAttribute(k, String(v))
  return c
}

function empty(fill: SVGCircleElement): void {
  fill.setAttribute('stroke-dashoffset', '0')
  fill.setAttribute('stroke-dasharray', `0 ${C}`)
  fill.style.opacity = '0'
}

/** A ring leaves the arc and fades: one, or a pair a beat apart. */
export function ripple(face: Element, count = 1): void {
  const svg = svgOf(face)
  if (still() || !svg) return
  for (let i = 0; i < count; i++) {
    const r = circle({
      cx: 160, cy: 160, r: 148, fill: 'none', stroke: 'var(--arc-sun)', 'stroke-width': 2, opacity: 0.5,
      'stroke-dasharray': `${SPAN} ${C}`, transform: 'rotate(150 160 160)', 'stroke-linecap': 'round',
    })
    svg.append(r)
    gsap.to(r, {
      attr: { r: 186, 'stroke-width': 0.5 }, opacity: 0, duration: 0.9, delay: i * 0.16, ease: 'power2.out',
      onComplete: () => r.remove(),
    })
  }
}

/** The fill runs to the end, the ends meet, and bead `beadIndex` of `total` drops into the opening. */
export function closeRing(face: Element, fill: SVGCircleElement, beadIndex = 0, total = 4): gsap.core.Timeline {
  const tl = gsap.timeline()
  if (still()) {
    fill.setAttribute('stroke-dasharray', `${SPAN} ${C}`)
    tl.fromTo(fill, { opacity: 0 }, { opacity: 1, duration: 0.2 }, 0)
    bead(face, beadIndex, total)
    return tl
  }
  fill.style.opacity = '1'
  tl.to(fill, { attr: { 'stroke-dasharray': `${SPAN} ${C}` }, duration: 0.55, ease: 'power3.inOut' }, 0)
  tl.to(fill, { attr: { 'stroke-width': 11 }, duration: 0.18, yoyo: true, repeat: 1, ease: 'sine.inOut' }, 0.45)
  tl.add(() => bead(face, beadIndex, total), 0.5)
  tl.add(() => ripple(face, 1), 0.5)
  return tl
}

/** A round's bead lands in the ring's opening, closing the circle a little more. */
export function bead(face: Element, i: number, total = 4): void {
  const svg = svgOf(face)
  if (!svg) return
  const { x, y } = beadAt(i, total)
  if (still()) {
    svg.append(circle({ cx: x, cy: y, r: 4.5, fill: 'var(--arc-sun)', class: 'bead' }))
    return
  }
  const b = circle({ cx: x, cy: y - 18, r: 4.5, fill: 'var(--arc-sun)', opacity: 0, class: 'bead' })
  svg.append(b)
  gsap.to(b, { attr: { cy: y }, opacity: 1, duration: 0.5, ease: 'back.out(2.2)' })
}

/** Quiet beads in the opening; the first `done` lit for rounds already done today. */
export function beadsAtRest(face: Element, done: number, total = 4): void {
  const svg = svgOf(face)
  if (!svg) return
  for (let i = 0; i < total; i++) {
    const { x, y } = beadAt(i, total)
    svg.append(circle({ cx: x, cy: y, r: 4.5, fill: i < done ? 'var(--arc-sun)' : 'var(--track)', class: 'bead' }))
  }
}

/** The arc draws clockwise, the track appears beneath it, and the arc unwinds from its tail; `done` runs once the fill is empty and ready to count. */
export function open(
  _face: Element,
  fill: SVGCircleElement,
  track: SVGCircleElement,
  done?: () => void,
): gsap.core.Timeline | undefined {
  if (still()) {
    gsap.set(track, { opacity: 1 })
    empty(fill)
    done?.()
    return
  }
  const s = { len: 0, cut: 0 }
  const paint = () => {
    const len = Math.max(0, s.len - s.cut)
    fill.setAttribute('stroke-dasharray', `${len} ${C}`)
    fill.setAttribute('stroke-dashoffset', String(-s.cut))
    fill.style.opacity = len > 9 ? '1' : '0'
  }
  gsap.set(track, { opacity: 0 })
  const tl = gsap.timeline({ onUpdate: paint, onComplete: () => { empty(fill); done?.() } })
  tl.to(s, { len: SPAN, duration: 0.7, ease: 'power2.inOut' }, 0)
  tl.to(track, { opacity: 1, duration: 0.5, ease: 'power1.out' }, 0.45)
  tl.to(s, { cut: SPAN, duration: 0.6, ease: 'power2.inOut' }, 0.75)
  return tl
}

/** A soft band of `color` widens out from the ring across `frame` and fades. */
export function wave(face: Element, frame: HTMLElement, color = 'var(--arc-sun)', peak = 0.18): void {
  if (still()) return
  const box = face.getBoundingClientRect(), root = frame.getBoundingClientRect()
  const cx = box.left - root.left + box.width / 2, cy = box.top - root.top + box.height / 2
  const w = document.createElement('div')
  w.style.cssText = 'position:absolute;inset:0;pointer-events:none;mix-blend-mode:multiply'
  frame.append(w)
  const s = { r: 150, o: peak }
  gsap.to(s, {
    r: 720, o: 0, duration: 1.6, ease: 'power1.out',
    onUpdate: () => {
      const band = 70 + (s.r - 150) * 0.45
      w.style.opacity = String(s.o)
      w.style.background = `radial-gradient(circle at ${cx}px ${cy}px, transparent ${s.r - band}px, ${color} ${s.r}px, transparent ${s.r + band}px)`
    },
    onComplete: () => w.remove(),
  })
}

/** The closed arc tints to sage and the `.home` ground dims a step (`veilOn`), ready for the break to drain. */
export function toBreak(face: Element, fill: SVGCircleElement, veilOn: boolean): gsap.core.Timeline {
  fill.style.transition = still() ? '' : 'stroke 700ms ease'
  fill.style.stroke = 'var(--arc-sage)'
  const tl = gsap.timeline()
  const home = homeOf(face)
  if (!home) return tl
  let veil = home.querySelector<HTMLElement>('.veil')
  if (!veil) {
    veil = document.createElement('div')
    veil.className = 'veil'
    veil.style.cssText = 'position:absolute;inset:0;pointer-events:none;background:oklch(30% 0.03 250 / 0.14);opacity:0'
    home.prepend(veil)
  }
  if (still()) tl.set(veil, { opacity: veilOn ? 1 : 0 }, 0)
  else tl.to(veil, { opacity: veilOn ? 1 : 0, duration: 0.7, ease: 'power1.inOut' }, 0)
  return tl
}

/** The break is over: the ground brightens, the sage and track fade, and the fill is empty and ready. */
export function toWork(face: Element, fill: SVGCircleElement, track?: SVGCircleElement): gsap.core.Timeline {
  const veil = homeOf(face)?.querySelector('.veil')
  const both = track ? [fill, track] : fill
  const reset = () => {
    fill.style.stroke = ''
    fill.setAttribute('stroke-dashoffset', '0')
    fill.setAttribute('stroke-dasharray', `0 ${C}`)
  }
  const tl = gsap.timeline()
  if (still()) {
    if (veil) tl.set(veil, { opacity: 0 }, 0)
    tl.set(both, { opacity: 0 }, 0)
    reset()
    return tl
  }
  if (veil) tl.to(veil, { opacity: 0, duration: 0.6, ease: 'power1.inOut' }, 0)
  tl.to(both, { opacity: 0, duration: 0.4 }, 0)
  tl.add(reset, 0.45)
  return tl
}

/** A break drains from the far end of the arc: `left` px of the span is still to go. */
export function drain(fill: SVGCircleElement, left: number): void {
  fill.setAttribute('stroke-dasharray', `${left} ${C}`)
  fill.setAttribute('stroke-dashoffset', String(-(SPAN - left)))
  fill.style.opacity = left > 9 ? '1' : '0'
}
