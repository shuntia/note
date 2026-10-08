import { JitterBuffer, Linear } from './pcm'

declare const sampleRate: number
declare const currentFrame: number
declare class AudioWorkletProcessor {
  readonly port: MessagePort
}
declare function registerProcessor(name: string, ctor: new () => AudioWorkletProcessor): void

type In = { pcm: ArrayBuffer; rate: number } | { flush: true }

class Playback extends AudioWorkletProcessor {
  private buffer = new JitterBuffer(Math.round(sampleRate * 0.12), Math.round(sampleRate * 0.04))
  private resample: Linear | null = null
  private level = 0
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
      this.buffer.push(this.resample.push(new Int16Array(m.pcm)), currentFrame)
    }
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const out = outputs[0][0]
    const rms = this.buffer.pull(out, currentFrame)
    for (let c = 1; c < outputs[0].length; c++) outputs[0][c].set(out)
    this.level = Math.max(rms, this.level * 0.9)
    this.sinceReport += out.length
    if (this.sinceReport >= sampleRate / 50) {
      this.sinceReport = 0
      this.port.postMessage({ level: this.level, buffered: this.buffer.buffered })
    }
    return true
  }
}

registerProcessor('note-playback', Playback)
