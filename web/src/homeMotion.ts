import { gsap } from 'gsap'
import { ScrollTrigger } from 'gsap/ScrollTrigger'

gsap.registerPlugin(ScrollTrigger)

export type Timeline = gsap.core.Timeline
export type Trigger = ScrollTrigger

type Rect = { left: number; top: number; width: number; height: number }
type Target = { r: Rect; whole?: boolean; text?: string; svg?: boolean }

// Drives a paused timeline from the scroll by hand so the catch-up can differ by
// direction: arriving is brisk, leaving lingers. The timeline is looked up on every
// update so it can be rebuilt without touching the trigger.
export function scrub(
  timeline: () => Timeline | null,
  trigger: ScrollTrigger.Vars,
  { down, up }: { down: number; up: number },
): Trigger {
  const follow = (self: Trigger, duration: number) => {
    const tl = timeline()
    if (tl) gsap.to(tl, { progress: self.progress, duration, ease: 'power2.out', overwrite: true })
  }
  return ScrollTrigger.create({
    ...trigger,
    onRefresh: (self) => {
      const tl = timeline()
      if (!tl) return
      gsap.killTweensOf(tl)
      tl.progress(self.progress)
    },
    onUpdate: (self) => follow(self, self.direction < 0 ? up : down),
  })
}

const SETTLE_MS = 500
const GESTURE_MS = 400
const TOUCH_THRESHOLD = 24
const AT_STOP = 2
const OVERLAY = '.sheet, .ev-menu, [role="dialog"]'

// Something under the pointer that can take the scroll itself keeps it.
function scrollableUnder(target: EventTarget | null, down: boolean): boolean {
  let el = target instanceof Element ? target : null
  while (el && el !== document.body && el !== document.documentElement) {
    if (el.matches(OVERLAY)) return true
    const overflow = getComputedStyle(el).overflowY
    if (
      (overflow === 'auto' || overflow === 'scroll') &&
      el.scrollHeight > el.clientHeight + 1 &&
      (down ? el.scrollTop + el.clientHeight < el.scrollHeight - 1 : el.scrollTop > 1)
    )
      return true
    el = el.parentElement
  }
  return false
}

// One gesture, one stop. While the page is at or inside the stage the wheel and the
// finger do not scroll it: they choose the next stop — the face, the stage, and past
// the stage the page again — and everything further is ignored until that scroll has
// settled and half a second of quiet has passed, so a trackpad's inertia or a flick
// counts once. Without motion the stops still hold; the page simply jumps to them.
export function clampStops(stops: () => { start: number; end: number }): () => void {
  let tween: gsap.core.Tween | null = null
  let quiet = 0
  let from: number | null = null
  let handled = 0
  let last = window.scrollY
  let aim: number | null = null

  const busy = () => tween !== null || performance.now() < quiet
  const hold = () => {
    quiet = performance.now() + SETTLE_MS
  }
  const go = (y: number) => {
    tween?.kill()
    aim = y
    if (Math.abs(y - window.scrollY) < 1) return hold()
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
      window.scrollTo(0, y)
      return hold()
    }
    tween = scrollToY(y)
    tween.eventCallback('onComplete', () => {
      tween = null
      hold()
    })
  }

  const inside = () => window.scrollY <= stops().end + AT_STOP

  // Down from anywhere above the stage's end lands on it; resting on the stage, up
  // goes to the face, and up from below lands on the stage first.
  const stop = (down: boolean): number | null => {
    const { start, end } = stops()
    const y = window.scrollY
    if (down) return y < end - AT_STOP ? end : null
    if (y >= end - AT_STOP) return start
    if (y > start + AT_STOP) return end
    return null
  }

  const onWheel = (e: WheelEvent) => {
    if (e.deltaY === 0) return
    handled = performance.now()
    const down = e.deltaY > 0
    if (!inside() || scrollableUnder(e.target, down)) return
    if (busy()) {
      e.preventDefault()
      hold()
      return
    }
    const to = stop(down)
    if (to === null) return
    e.preventDefault()
    go(to)
  }
  const onStart = (e: TouchEvent) => {
    from = e.touches.length === 1 ? e.touches[0].clientY : null
  }
  const onMove = (e: TouchEvent) => {
    if (from === null) return
    handled = performance.now()
    const moved = from - e.touches[0].clientY
    const down = moved > 0
    if (!inside() || scrollableUnder(e.target, down)) return
    if (busy()) {
      e.preventDefault()
      hold()
      return
    }
    if (Math.abs(moved) < TOUCH_THRESHOLD) return
    const to = stop(down)
    if (to === null) return
    e.preventDefault()
    from = e.touches[0].clientY
    go(to)
  }
  const onEnd = () => {
    from = null
  }
  // A single coarse wheel event can clear the whole stage; crossing its end on the way
  // up is the gesture that brings the page back to it.
  const onScroll = () => {
    const was = last
    last = window.scrollY
    // A fling the page had already taken when the gesture was caught would otherwise
    // drift off the stop while everything else is being ignored.
    if (busy()) {
      if (tween === null && aim !== null && Math.abs(last - aim) > 1) window.scrollTo(0, aim)
      return
    }
    if (performance.now() - handled > GESTURE_MS) return
    const { end } = stops()
    if (was > end + AT_STOP && last <= end + AT_STOP) go(end)
  }

  addEventListener('wheel', onWheel, { passive: false })
  addEventListener('scroll', onScroll, { passive: true })
  addEventListener('touchstart', onStart, { passive: true })
  addEventListener('touchmove', onMove, { passive: false })
  addEventListener('touchend', onEnd, { passive: true })
  addEventListener('touchcancel', onEnd, { passive: true })
  return () => {
    tween?.kill()
    removeEventListener('wheel', onWheel)
    removeEventListener('scroll', onScroll)
    removeEventListener('touchstart', onStart)
    removeEventListener('touchmove', onMove)
    removeEventListener('touchend', onEnd)
    removeEventListener('touchcancel', onEnd)
  }
}

export function scrollToY(y: number): gsap.core.Tween {
  const pos = { y: window.scrollY }
  return gsap.to(pos, {
    y,
    duration: 0.25 + 0.25 * Math.min(1, Math.abs(y - pos.y) / 600),
    ease: 'power2.inOut',
    onUpdate: () => window.scrollTo(0, pos.y),
  })
}

const rect = (el: Element): Rect => el.getBoundingClientRect()

// Where each word and glyph of an element sits, without touching it.
function measureAtoms(el: Element): Target[] {
  const out: Target[] = []
  const walk = (n: Node) => {
    if (n.nodeType === Node.TEXT_NODE) {
      const re = /\S+/g
      const text = n.nodeValue ?? ''
      let m: RegExpExecArray | null
      while ((m = re.exec(text))) {
        const rg = document.createRange()
        rg.setStart(n, m.index)
        rg.setEnd(n, m.index + m[0].length)
        out.push({ text: m[0], r: rg.getBoundingClientRect() })
      }
    } else if ((n as Element).tagName === 'svg') out.push({ svg: true, r: rect(n as Element) })
    else for (const c of n.childNodes) walk(c)
  }
  walk(el)
  return out
}

// Pairs the face's atoms with the target's: glyphs in order, words by their text;
// anything unmatched heads for the centre of the whole target and fades on the way.
function matchAtoms(atoms: HTMLElement[], targets: Target[], b: Element): [HTMLElement, Target][] {
  const free = [...targets]
  const whole: Target = { r: rect(b), whole: true }
  return atoms.map((el) => {
    const i = free.findIndex((t) => (el.tagName === 'svg' ? t.svg : t.text === el.textContent))
    return [el, i < 0 ? whole : free.splice(i, 1)[0]]
  })
}

export type TravelOptions = {
  // 'text': the face's `.atom` spans to the matching words of `b`; 'box': the whole
  // element; 'children': child to child by index.
  mode?: 'text' | 'box' | 'children'
  fit?: 'height' | 'both'
  swap?: number
}

// Carries the face's copy of something onto Today's copy and replaces it there: only
// `a` moves, piece by piece, taking on `b`'s letter-spacing and optical size on the
// way, and at the very end `a` gives way to `b`. Positions are read before anything
// moves, so `a` and `b` must both be laid out untransformed when this runs.
export function travel(
  tl: Timeline,
  a: HTMLElement | null,
  b: HTMLElement | null,
  { mode = 'text', fit = 'height', swap = 0.985 }: TravelOptions = {},
): void {
  if (!a || !b) return
  const atoms = mode === 'text' ? [...a.querySelectorAll<HTMLElement>('.atom')] : []
  if (mode === 'text' && !atoms.length) mode = 'box'
  let pairs: [HTMLElement, Target][]
  if (mode === 'box') pairs = [[a, { r: rect(b) }]]
  else if (mode === 'children') {
    pairs = []
    ;[...a.children].forEach((c, i) => {
      const to = b.children[i]
      if (to) pairs.push([c as HTMLElement, { r: rect(to) }])
    })
  } else pairs = matchAtoms(atoms, measureAtoms(b), b)

  const fontA = parseFloat(getComputedStyle(a).fontSize)
  const fontB = parseFloat(getComputedStyle(b).fontSize)
  const fontScale = fontB / fontA
  const lsA = (parseFloat(getComputedStyle(a).letterSpacing) || 0) / fontA
  const lsB = (parseFloat(getComputedStyle(b).letterSpacing) || 0) / fontB
  const words = mode === 'text' ? pairs.map(([el]) => el).filter((el) => el.tagName !== 'svg') : []
  const setType = (opsz: number, ls: number) =>
    words.forEach((el) => gsap.set(el, { '--opsz': opsz, '--ls': `${ls.toFixed(4)}em` }))
  // Where each word will sit is measured with the target's metrics in place, since
  // they change its width; then everything is put back to start from.
  setType(fontB, lsB)
  const from = new Map(pairs.map(([el]) => [el, rect(el)]))
  setType(fontA, lsA)
  for (const [el, to] of pairs) {
    const f = from.get(el)!
    const word = words.includes(el)
    let sx: number
    let sy: number
    if (word) sx = sy = fontScale
    else {
      sy = to.r.height / f.height
      sx = fit === 'both' ? to.r.width / f.width : sy
    }
    const vars: gsap.TweenVars = {
      x: to.r.left + to.r.width / 2 - (f.left + f.width / 2),
      y: to.r.top + to.r.height / 2 - (f.top + f.height / 2),
      scaleX: sx,
      scaleY: sy,
      transformOrigin: '50% 50%',
      duration: 1,
      ease: 'power2.inOut',
    }
    if (word) {
      vars['--opsz'] = fontB
      vars['--ls'] = `${lsB.toFixed(4)}em`
    }
    tl.to(el, vars, 0)
    if (to.whole) tl.to(el, { autoAlpha: 0, duration: 0.5, ease: 'none' }, 0.25)
  }
  tl.to(a, { autoAlpha: 0, duration: 1 - swap, ease: 'none' }, swap).from(b, { autoAlpha: 0, duration: 1 - swap, ease: 'none' }, swap)
}

// The panels below start unseen and fade in softly as they scroll up into view; on
// the way back to the very top they fade out again, more slowly than they came.
export function scrollReveal(build: (tl: Timeline) => void, trigger: ScrollTrigger.Vars): () => void {
  const tl = gsap.timeline({ paused: true })
  build(tl)
  const st = scrub(() => tl, { start: 'top bottom', end: 'top 45%', ...trigger }, { down: 0.9, up: 1.8 })
  return () => {
    st.kill()
    clearTimeline(tl)
  }
}

// Undoes everything a timeline set on its elements (and only that, so the inline
// sizes React gave them stay), so the pieces can be measured afresh. Transforms go
// through gsap so its per-element transform cache is reset with the style.
export function clearTimeline(tl: Timeline): void {
  gsap.killTweensOf(tl)
  tl.progress(0)
  const targets = tl.getChildren().flatMap((t) => (t as gsap.core.Tween).targets?.() ?? [])
  tl.kill()
  for (const t of targets) {
    if (!(t instanceof Element)) continue
    gsap.set(t, { clearProps: 'transform,opacity,visibility' })
    ;(t as HTMLElement).style.removeProperty('--opsz')
    ;(t as HTMLElement).style.removeProperty('--ls')
  }
}
