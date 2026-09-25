// Subtle celebrations on the arc. Every effect takes the face element (the 320
// box that holds the arc svg) and returns nothing; gsap is a global.
const NS = 'http://www.w3.org/2000/svg'
const svgOf = (face) => face.querySelector('.arc svg')
const circle = (attrs) => { const c = document.createElementNS(NS, 'circle'); for (const [k, v] of Object.entries(attrs)) c.setAttribute(k, v); return c }
const C = 2 * Math.PI * 148, SPAN = C * (240 / 360)
const headAt = (frac) => { const a = (150 + 90 + 240 * frac) * Math.PI / 180; return { x: 160 + 148 * Math.cos(a), y: 160 + 148 * Math.sin(a) } }

// A ring leaves the arc and fades: one, or a pair a beat apart.
export function ripple(face, count = 1) {
  const svg = svgOf(face)
  for (let i = 0; i < count; i++) {
    const r = circle({ cx: 160, cy: 160, r: 148, fill: 'none', stroke: 'var(--sun)', 'stroke-width': 2, opacity: 0.5, 'stroke-dasharray': `${SPAN} ${C}`, transform: 'rotate(150 160 160)', 'stroke-linecap': 'round' })
    svg.append(r)
    gsap.to(r, { attr: { r: 186, 'stroke-width': 0.5 }, opacity: 0, duration: 0.9, delay: i * 0.16, ease: 'power2.out', onComplete: () => r.remove() })
  }
}

// A flare where the arc begins.
export function ignite(face, frac = 0) {
  const svg = svgOf(face)
  const { x, y } = headAt(frac)
  const flare = circle({ cx: x, cy: y, r: 6, fill: 'var(--sun)', opacity: 0.7 })
  svg.append(flare)
  gsap.to(flare, { attr: { r: 26 }, opacity: 0, duration: 0.7, ease: 'power2.out', onComplete: () => flare.remove() })
}

// The whole face draws a breath: in from slightly small and dim, a faint warmth
// behind it that fades.
export function breath(face) {
  gsap.fromTo(face, { scale: 0.965, opacity: 0.55 }, { scale: 1, opacity: 1, duration: 0.7, ease: 'power2.out', clearProps: 'transform,opacity' })
  glow(face, 0.35)
}

function glow(face, peak) {
  let g = face.querySelector('.glow')
  if (!g) { g = document.createElement('div'); g.className = 'glow'; g.style.cssText = 'position:absolute;inset:-40px;border-radius:50%;pointer-events:none;background:radial-gradient(circle at 50% 50%, var(--sun) 0%, transparent 62%);opacity:0'; face.prepend(g) }
  gsap.fromTo(g, { opacity: 0 }, { opacity: peak, duration: 0.3, ease: 'power1.out', yoyo: true, repeat: 1, repeatDelay: 0.15 })
}

// The fill runs to the end, the two ends meet, and a bead drops into the opening.
export function closeRing(face, fill, beadIndex = 0) {
  const svg = svgOf(face)
  const tl = gsap.timeline()
  fill.style.opacity = 1
  tl.to(fill, { attr: { 'stroke-dasharray': `${SPAN} ${C}` }, duration: 0.55, ease: 'power3.inOut' }, 0)
  tl.to([fill], { attr: { 'stroke-width': 11 }, duration: 0.18, yoyo: true, repeat: 1, ease: 'sine.inOut' }, 0.45)
  tl.add(() => bead(face, beadIndex), 0.5)
  tl.add(() => ripple(face, 1), 0.5)
  return tl
}

// Beads continue the ring across its opening (30° to 150°, the bottom), evenly.
const beadAt = (i, total) => { const a = (30 + (120 * (i + 1)) / (total + 1)) * Math.PI / 180; return { x: 160 + 148 * Math.cos(a), y: 160 + 148 * Math.sin(a) } }

// A round's bead lands in the ring's opening at the bottom, so each round closes
// the circle a little more.
export function bead(face, i, total = 4) {
  const svg = svgOf(face)
  const { x, y } = beadAt(i, total)
  const b = circle({ cx: x, cy: y - 18, r: 4.5, fill: 'var(--sun)', opacity: 0, class: 'bead' })
  svg.append(b)
  gsap.to(b, { attr: { cy: y }, opacity: 1, duration: 0.5, ease: 'back.out(2.2)' })
}

// The ground warms for a moment, like a cloud passing off the sun.
export function warm(face) {
  const app = face.closest('.frame-app')
  let w = app.querySelector('.warm')
  if (!w) { w = document.createElement('div'); w.className = 'warm'; w.style.cssText = 'position:absolute;inset:0;pointer-events:none;background:radial-gradient(circle at 50% 40%, var(--sun) 0%, transparent 70%);opacity:0'; app.append(w) }
  gsap.fromTo(w, { opacity: 0 }, { opacity: 0.16, duration: 0.25, ease: 'power1.out', yoyo: true, repeat: 1, repeatDelay: 0.2 })
}

// Quiet beads in the opening for rounds already done today.
export function beadsAtRest(face, done, total = 4) {
  const svg = svgOf(face)
  for (let i = 0; i < total; i++) {
    const { x, y } = beadAt(i, total)
    svg.append(circle({ cx: x, cy: y, r: 4.5, fill: i < done ? 'var(--sun)' : 'var(--track)', class: 'bead' }))
  }
}

// The arc draws clockwise from where it begins, the grey track appears beneath
// it, and the arc unwinds again from its tail: the fill is then ready to count.
// `done` runs when it is.
export function open(face, fill, track, done) {
  const s = { len: 0, cut: 0 }
  const paint = () => {
    const len = Math.max(0, s.len - s.cut)
    fill.setAttribute('stroke-dasharray', `${len} ${C}`)
    fill.setAttribute('stroke-dashoffset', -s.cut)
    fill.style.opacity = len > 9 ? 1 : 0
  }
  gsap.set(track, { opacity: 0 })
  const tl = gsap.timeline({ onUpdate: paint, onComplete: () => { fill.setAttribute('stroke-dashoffset', 0); fill.setAttribute('stroke-dasharray', `0 ${C}`); fill.style.opacity = 0; done?.() } })
  tl.to(s, { len: SPAN, duration: 0.7, ease: 'power2.inOut' }, 0)
  tl.to(track, { opacity: 1, duration: 0.5, ease: 'power1.out' }, 0.45)
  tl.to(s, { cut: SPAN, duration: 0.6, ease: 'power2.inOut' }, 0.75)
  return tl
}

// A soft band of the arc's own colour widens out from the ring and fades: the
// ripple as a gradient rather than a line.
export function wave(face, color = 'var(--sun)', peak = 0.18) {
  const app = face.closest('.frame-app')
  const box = face.getBoundingClientRect(), root = app.getBoundingClientRect()
  const cx = box.left - root.left + box.width / 2, cy = box.top - root.top + box.height / 2
  const w = document.createElement('div')
  w.style.cssText = 'position:absolute;inset:0;pointer-events:none;mix-blend-mode:multiply'
  app.append(w)
  const s = { r: 150, o: peak }
  gsap.to(s, { r: 720, o: 0, duration: 1.6, ease: 'power1.out', onUpdate: () => {
    const band = 70 + (s.r - 150) * 0.45
    w.style.opacity = s.o
    w.style.background = `radial-gradient(circle at ${cx}px ${cy}px, transparent ${s.r - band}px, ${color} ${s.r}px, transparent ${s.r + band}px)`
  }, onComplete: () => w.remove() })
}

// The round is done: the closed sun arc tints to sage, the ground dims a step, and
// the break is ready to drain from the far end. Returns the timeline.
export function toBreak(face, fill, veilOn) {
  fill.style.transition = 'stroke 700ms ease'
  fill.style.stroke = 'var(--sage)'
  const app = face.closest('.frame-app')
  let veil = app.querySelector('.veil')
  if (!veil) { veil = document.createElement('div'); veil.className = 'veil'; veil.style.cssText = 'position:absolute;inset:0;pointer-events:none;background:oklch(30% 0.03 250 / 0.14);opacity:0'; app.prepend(veil) }
  const tl = gsap.timeline()
  tl.to(veil, { opacity: veilOn ? 1 : 0, duration: 0.7, ease: 'power1.inOut' }, 0)
  return tl
}

// The break is over: the ground brightens, the last of the sage goes, and the sun
// arc is empty and ready.
export function toWork(face, fill) {
  const app = face.closest('.frame-app')
  const veil = app.querySelector('.veil')
  const tl = gsap.timeline()
  if (veil) tl.to(veil, { opacity: 0, duration: 0.6, ease: 'power1.inOut' }, 0)
  tl.to(fill, { opacity: 0, duration: 0.4 }, 0)
  tl.add(() => { fill.style.stroke = 'var(--sun)'; fill.setAttribute('stroke-dashoffset', 0); fill.setAttribute('stroke-dasharray', `0 ${C}`) }, 0.45)
  return tl
}

// A break drains from the far end of the arc: `left` of the span is still to go.
export function drain(fill, left) {
  fill.setAttribute('stroke-dasharray', `${left} ${C}`)
  fill.setAttribute('stroke-dashoffset', -(SPAN - left))
  fill.style.opacity = left > 9 ? 1 : 0
}
