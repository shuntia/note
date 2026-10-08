export const CAPTURE_RATE = 16_000
export const CAPTURE_FRAME = 320

const toS16 = (v: number) => Math.max(-32768, Math.min(32767, Math.round(v * 32767)))

// Box-filters and decimates the mic to 16 kHz, handing back each whole 20 ms frame.
export class Downsampler {
  private buf: number[] = []
  private pos: number
  private out = new Int16Array(CAPTURE_FRAME)
  private n = 0
  private readonly ratio: number

  constructor(inRate: number) {
    this.ratio = inRate / CAPTURE_RATE
    this.pos = this.ratio / 2
  }

  push(input: Float32Array): Int16Array[] {
    for (let i = 0; i < input.length; i++) this.buf.push(input[i])
    const half = this.ratio / 2
    const frames: Int16Array[] = []
    while (this.pos + half <= this.buf.length) {
      const lo = Math.max(0, Math.floor(this.pos - half))
      const hi = Math.min(this.buf.length, Math.ceil(this.pos + half))
      let sum = 0
      for (let i = lo; i < hi; i++) sum += this.buf[i]
      this.out[this.n++] = toS16(sum / (hi - lo))
      if (this.n === CAPTURE_FRAME) {
        frames.push(this.out)
        this.out = new Int16Array(CAPTURE_FRAME)
        this.n = 0
      }
      this.pos += this.ratio
    }
    const drop = Math.max(0, Math.floor(this.pos - half))
    if (drop > 0) {
      this.buf.splice(0, drop)
      this.pos -= drop
    }
    return frames
  }
}

// Linear interpolation from one rate to another; `pos` indexes [last sample of the previous chunk, ...this chunk].
export class Linear {
  private prev = 0
  private pos = 1

  constructor(
    readonly from: number,
    readonly to: number,
  ) {}

  push(input: Int16Array): Float32Array {
    if (this.from === this.to) return Float32Array.from(input, (v) => v / 32768)
    const step = this.from / this.to
    const n = input.length
    const at = (i: number) => (i === 0 ? this.prev : input[i - 1]) / 32768
    const out: number[] = []
    while (this.pos < n) {
      const i = Math.floor(this.pos)
      const f = this.pos - i
      out.push(at(i) * (1 - f) + at(i + 1) * f)
      this.pos += step
    }
    this.pos -= n
    if (n > 0) this.prev = input[n - 1]
    return Float32Array.from(out)
  }

  reset() {
    this.prev = 0
    this.pos = 1
  }
}

export class JitterBuffer {
  private chunks: Float32Array[] = []
  private offset = 0
  private size = 0
  private primed = false
  private lastPush = -Infinity

  constructor(
    private readonly target: number,
    private readonly idle: number,
  ) {}

  push(x: Float32Array, now: number) {
    if (x.length === 0) return
    this.chunks.push(x)
    this.size += x.length
    this.lastPush = now
    if (this.size >= this.target) this.primed = true
  }

  pull(out: Float32Array, now: number): number {
    if (!this.primed && this.size > 0 && now - this.lastPush >= this.idle) this.primed = true
    if (!this.primed) {
      out.fill(0)
      return 0
    }
    let i = 0
    let sum = 0
    while (i < out.length && this.chunks.length > 0) {
      const c = this.chunks[0]
      const n = Math.min(out.length - i, c.length - this.offset)
      for (let k = 0; k < n; k++) {
        const v = c[this.offset + k]
        out[i + k] = v
        sum += v * v
      }
      i += n
      this.offset += n
      this.size -= n
      if (this.offset === c.length) {
        this.chunks.shift()
        this.offset = 0
      }
    }
    if (i < out.length) {
      out.fill(0, i)
      this.primed = false
    }
    return Math.sqrt(sum / out.length)
  }

  flush() {
    this.chunks = []
    this.offset = 0
    this.size = 0
    this.primed = false
  }

  get buffered() {
    return this.size
  }
}

export function rms16(x: Int16Array): number {
  if (x.length === 0) return 0
  let s = 0
  for (let i = 0; i < x.length; i++) s += (x[i] / 32768) ** 2
  return Math.sqrt(s / x.length)
}
