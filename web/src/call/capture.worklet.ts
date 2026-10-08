import { Downsampler } from './pcm'

declare const sampleRate: number
declare class AudioWorkletProcessor {
  readonly port: MessagePort
}
declare function registerProcessor(name: string, ctor: new () => AudioWorkletProcessor): void

class Capture extends AudioWorkletProcessor {
  private down = new Downsampler(sampleRate)

  process(inputs: Float32Array[][]): boolean {
    const channel = inputs[0]?.[0]
    if (channel) for (const frame of this.down.push(channel)) this.port.postMessage(frame, [frame.buffer])
    return true
  }
}

registerProcessor('note-capture', Capture)
