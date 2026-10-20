import { useLayoutEffect, useRef } from 'react'
import { reducedMotion } from './motion'

export const FADE_MS = 240
const STEP_MS = 24
// A burst's words all start within this, however many there are.
const SPREAD_MS = 120
// How far a new burst may queue behind the one still staggering in.
const LAG_MS = 60
const MAX_SEGMENTS = 40
// Longer than this on mount, the text was already there rather than arriving.
const MOUNT_FADE = 240

export type Segment = { from: number; to: number; start: number }

// Words keep their leading whitespace, so the tokens always rejoin to the text.
export const words = (text: string): string[] => text.match(/\s*\S+|\s+$/g) ?? []

/** `text`, which starts at offset `at`, as words fading in from `now`, queued after a burst starting at `after`. */
export function schedule(text: string, at: number, now: number, after = -Infinity): Segment[] {
  const tokens = words(text)
  if (!tokens.length) return []
  const per = Math.ceil(tokens.length / MAX_SEGMENTS)
  const groups: string[] = []
  for (let i = 0; i < tokens.length; i += per) groups.push(tokens.slice(i, i + per).join(''))
  const step = groups.length > 1 ? Math.min(STEP_MS, SPREAD_MS / (groups.length - 1)) : 0
  const first = Math.max(now, Math.min(after + STEP_MS, now + LAG_MS))
  let from = at
  return groups.map((g, i) => {
    const seg = { from, to: from + g.length, start: first + i * step }
    from = seg.to
    return seg
  })
}

export type Piece = { node: number; from: number; to: number; delay: number }

/** Where each segment falls in a run of text nodes, last piece of each node first so splitting keeps offsets valid. */
export function pieces(lengths: number[], segs: Segment[], now: number): Piece[] {
  const out: Piece[] = []
  let a = 0
  lengths.forEach((len, node) => {
    const b = a + len
    const here: Piece[] = []
    for (const s of segs) {
      const from = Math.max(s.from, a)
      const to = Math.min(s.to, b)
      if (from < to) here.push({ node, from: from - a, to: to - a, delay: s.start - now })
    }
    out.push(...here.reverse())
    a = b
  })
  return out
}

function textNodes(root: HTMLElement): Text[] {
  const walk = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => (n.parentElement?.closest('button') ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
  })
  const out: Text[] = []
  for (let n = walk.nextNode(); n; n = walk.nextNode()) out.push(n as Text)
  return out
}

type Fading = { span: HTMLElement; end: number }

function wrap(root: HTMLElement, segs: Segment[], now: number): Fading[] {
  const nodes = textNodes(root)
  const out: Fading[] = []
  for (const p of pieces(nodes.map((n) => n.data.length), segs, now)) {
    const node = nodes[p.node]
    if (p.to < node.data.length) node.splitText(p.to)
    const piece = p.from > 0 ? node.splitText(p.from) : node
    const span = document.createElement('span')
    span.className = 'stream-in'
    span.style.animationDuration = `${FADE_MS}ms`
    span.style.animationDelay = `${Math.round(p.delay)}ms`
    piece.replaceWith(span)
    span.append(piece)
    out.push({ span, end: now + p.delay + FADE_MS })
  }
  return out
}

function unwrap(spans: HTMLElement[]) {
  const parents = new Set<Node>()
  for (const span of spans) {
    if (!span.parentNode) continue
    parents.add(span.parentNode)
    span.replaceWith(...span.childNodes)
  }
  for (const p of parents) p.normalize()
}

/**
 * Fades text in word by word as it grows. `plain` owns the element's text and only
 * appends; `rendered` follows markup someone else rebuilds on every change, so the
 * words still fading are wrapped again, mid-animation, each time. Spans fold back
 * into plain text once the last one has finished.
 */
export class Fader {
  private text: string | null = null
  private shown = 0
  private segs: Segment[] = []
  private fading: Fading[] = []
  private timer = 0

  constructor(
    private root: HTMLElement,
    private mode: 'plain' | 'rendered',
  ) {}

  update(text: string) {
    const prev = this.text
    this.text = text
    const now = performance.now()
    const grows = prev !== null && text.startsWith(prev)
    if (this.mode === 'plain') {
      this.fold(now)
      if (grows) {
        if (text.length > prev.length) this.root.append(text.slice(prev.length))
      } else {
        this.root.textContent = text
        this.fading = []
        this.segs = []
        this.shown = prev === null && text.length <= MOUNT_FADE ? 0 : text.length
      }
    } else {
      this.fold(Infinity)
      if (!grows) {
        this.segs = []
        this.shown = prev === null && text.length <= MOUNT_FADE ? 0 : Infinity
      }
    }
    const full = this.mode === 'plain' ? text : textNodes(this.root).map((n) => n.data).join('')
    const from = Math.min(this.shown, full.length)
    this.shown = full.length
    if (reducedMotion()) {
      this.segs = []
      return
    }
    const live = this.segs.filter((s) => s.start + FADE_MS > now)
    const fresh = from < full.length ? schedule(full.slice(from), from, now, live.at(-1)?.start) : []
    this.segs = [...live, ...fresh]
    this.fading.push(...wrap(this.root, this.mode === 'plain' ? fresh : this.segs, now))
    if (!fresh.length) return
    window.clearTimeout(this.timer)
    const end = fresh[fresh.length - 1].start + FADE_MS
    this.timer = window.setTimeout(() => this.fold(Infinity), end - now + 50)
  }

  // Spans whose fade has ended by `now` go back to being plain text.
  private fold(now: number) {
    const done = this.fading.filter((f) => f.end <= now)
    if (!done.length) return
    this.fading = this.fading.filter((f) => f.end > now)
    unwrap(done.map((f) => f.span))
  }

  dispose() {
    window.clearTimeout(this.timer)
    this.fold(Infinity)
  }
}

/** Plain text that fades in as it streams; React never renders its children. */
export function StreamText({ text, className }: { text: string; className?: string }) {
  const ref = useRef<HTMLParagraphElement>(null)
  const fader = useRef<Fader | null>(null)
  useLayoutEffect(() => {
    const f = new Fader(ref.current!, 'plain')
    fader.current = f
    return () => {
      f.dispose()
      fader.current = null
    }
  }, [])
  useLayoutEffect(() => fader.current?.update(text), [text])
  return <p className={className} ref={ref} />
}
