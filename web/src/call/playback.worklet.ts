import { JitterBuffer, Linear } from './pcm'

declare const sampleRate: number
declare const currentFrame: number
declare class AudioWorkletProcessor {
  readonly port: MessagePort
}
declare function registerProcessor(name: string, ctor: new () => AudioWorkletProcessor): void

const REPORT_EVERY = sampleRate / 50

type In = { pcm: ArrayBuffer; rate: number } | { flush: true }

class Playback extends AudioWorkletProcessor {
  private buffer = new JitterBuffer(Math.round(sampleRate * 0.12), Math.round(sampleRate * 0.04))
  private resample: Linear | null = null
  private peak = 0
  private sinceReport = 0

  constructor() {
    super()
    this.port.onmessage = (e: MessageEvent<In>) => {
      const m = e.data
      if ('flush' in m) {
        this.buffer.flush()
        this.resample?.reset()
        return
      }
      if (!this.resample || this.resample.from !== m.rate) this.resample = new Linear(m.rate, sampleRate)
      this.buffer.push(this.resample.push(new Int16Array(m.pcm, 0, m.pcm.byteLength >> 1)), currentFrame)
    }
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const out = outputs[0][0]
    const rms = this.buffer.pull(out, currentFrame)
    for (let c = 1; c < outputs[0].length; c++) outputs[0][c].set(out)
    this.peak = Math.max(this.peak, rms)
    this.sinceReport += out.length
    if (this.sinceReport >= REPORT_EVERY) {
      this.sinceReport -= REPORT_EVERY
      this.port.postMessage({ level: this.peak, buffered: this.buffer.buffered })
      this.peak = 0
    }
    return true
  }
}

registerProcessor('note-playback', Playback)
