export const CAPTURE_RATE = 16_000
export const CAPTURE_FRAME = 320

const toS16 = (v: number) => Math.max(-32768, Math.min(32767, Math.round(v * 32767)))

const CUTOFF = 7_200

function lowPass(rate: number): Float64Array {
  const n = Math.max(32, 2 * Math.round(rate / 3000))
  const fc = CUTOFF / rate
  const mid = (n - 1) / 2
  const taps = Float64Array.from({ length: n }, (_, i) => {
    const t = i - mid
    const sinc = Math.sin(2 * Math.PI * fc * t) / (Math.PI * t)
    const w = (2 * Math.PI * i) / (n - 1)
    return sinc * (0.42 - 0.5 * Math.cos(w) + 0.08 * Math.cos(2 * w))
  })
  const sum = taps.reduce((a, v) => a + v, 0)
  return taps.map((v) => v / sum)
}

// Low-passes the mic below 8 kHz, reads it at 16 kHz with linear interpolation, and hands back each whole 20 ms frame.
export class Downsampler {
  private readonly taps: Float64Array
  private readonly history: Float64Array
  private head = 0
  private filtered: number[] = []
  private origin = 0
  private k = 0
  private out = new Int16Array(CAPTURE_FRAME)
  private n = 0

  constructor(private readonly inRate: number) {
    this.taps = lowPass(inRate)
    this.history = new Float64Array(this.taps.length)
  }

  push(input: Float32Array): Int16Array[] {
    const taps = this.taps
    const len = taps.length
    for (let i = 0; i < input.length; i++) {
      this.history[this.head] = input[i]
      let y = 0
      for (let j = 0, h = this.head; j < len; j++) {
        y += taps[j] * this.history[h]
        h = h === 0 ? len - 1 : h - 1
      }
      this.head = this.head + 1 === len ? 0 : this.head + 1
      this.filtered.push(y)
    }
    const frames: Int16Array[] = []
    for (;;) {
      const pos = this.origin + (this.k * this.inRate) / CAPTURE_RATE
      const i = Math.floor(pos)
      if (i + 1 >= this.filtered.length) break
      const f = pos - i
      this.out[this.n++] = toS16(this.filtered[i] * (1 - f) + this.filtered[i + 1] * f)
      if (this.n === CAPTURE_FRAME) {
        frames.push(this.out)
        this.out = new Int16Array(CAPTURE_FRAME)
        this.n = 0
      }
      if (++this.k === CAPTURE_RATE) {
        this.k = 0
        this.origin += this.inRate
      }
    }
    const drop = Math.min(this.filtered.length, Math.floor(this.origin + (this.k * this.inRate) / CAPTURE_RATE))
    if (drop > 0) {
      this.filtered.splice(0, drop)
      this.origin -= drop
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
    if (this.size > 3 * this.target) this.discard(this.size - this.target)
    if (this.size >= this.target) this.primed = true
  }

  private discard(count: number) {
    while (count > 0) {
      const c = this.chunks[0]
      const n = Math.min(count, c.length - this.offset)
      count -= n
      this.size -= n
      this.offset += n
      if (this.offset === c.length) {
        this.chunks.shift()
        this.offset = 0
      }
    }
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
