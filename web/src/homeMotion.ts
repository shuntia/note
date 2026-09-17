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

// Let go partway and the scroll itself settles on the nearer state, 80 ms after the
// last wheel, touch or scroll; any new input kills a settle already under way.
export function snapNearest(st: Trigger): () => void {
  let timer = 0
  let tween: gsap.core.Tween | null = null
  const go = () => {
    const p = st.progress
    if (!st.isActive || p <= 0 || p >= 1) return
    const target = p < 0.5 ? st.start : st.end
    const pos = { y: window.scrollY }
    tween = gsap.to(pos, {
      y: target,
      duration: 0.25 + 0.25 * (Math.abs(target - pos.y) / (st.end - st.start)),
      ease: 'power2.inOut',
      onUpdate: () => window.scrollTo(0, pos.y),
      onComplete: () => {
        tween = null
      },
    })
  }
  const arm = () => {
    window.clearTimeout(timer)
    timer = window.setTimeout(go, 80)
  }
  const input = () => {
    if (tween) {
      tween.kill()
      tween = null
    }
    arm()
  }
  const onScroll = () => {
    if (!tween) arm()
  }
  addEventListener('wheel', input, { passive: true })
  addEventListener('touchstart', input, { passive: true })
  addEventListener('touchend', arm, { passive: true })
  addEventListener('scroll', onScroll, { passive: true })
  return () => {
    window.clearTimeout(timer)
    tween?.kill()
    removeEventListener('wheel', input)
    removeEventListener('touchstart', input)
    removeEventListener('touchend', arm)
    removeEventListener('scroll', onScroll)
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
