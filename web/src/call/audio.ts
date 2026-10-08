import captureUrl from './capture.worklet.ts?worker&url'
import playbackUrl from './playback.worklet.ts?worker&url'
import { rms16 } from './pcm'
import { takeAudioContext } from './prime'
import type { AudioIo } from './session'

const REPORT_LAG_MS = 100

export function webAudio(): AudioIo {
  const ctx = takeAudioContext()
  let stream: MediaStream | null = null
  let source: MediaStreamAudioSourceNode | null = null
  let closed = false
  let capture: AudioWorkletNode | null = null
  let playback: AudioWorkletNode | null = null
  let mic = 0
  let out = 0
  let buffered = 0
  let lastPlay = -Infinity
  const stopMic = () => {
    stream?.getTracks().forEach((track) => track.stop())
    stream = null
    source?.disconnect()
    source = null
    capture?.disconnect()
    capture = null
    mic = 0
  }
  return {
    async startMic(onFrame) {
      const ensureOpen = () => {
        if (closed) {
          stopMic()
          throw new Error('closed')
        }
      }
      stream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true, channelCount: 1 },
      })
      ensureOpen()
      await Promise.all([ctx.audioWorklet.addModule(captureUrl), ctx.audioWorklet.addModule(playbackUrl)])
      ensureOpen()
      await ctx.resume()
      ensureOpen()
      playback = new AudioWorkletNode(ctx, 'note-playback', { numberOfInputs: 0, outputChannelCount: [1] })
      playback.port.onmessage = (e: MessageEvent<{ level: number; buffered: number }>) => {
        out = e.data.level
        buffered = e.data.buffered
      }
      playback.connect(ctx.destination)
      capture = new AudioWorkletNode(ctx, 'note-capture', { numberOfOutputs: 0 })
      capture.port.onmessage = (e: MessageEvent<Int16Array>) => {
        mic = rms16(e.data)
        onFrame(e.data)
      }
      source = ctx.createMediaStreamSource(stream)
      source.connect(capture)
    },
    stopMic,
    play(pcm, rate) {
      lastPlay = performance.now()
      playback?.port.postMessage({ pcm: pcm.buffer, rate }, [pcm.buffer as ArrayBuffer])
    },
    flush() {
      playback?.port.postMessage({ flush: true })
    },
    levels: () => ({ mic, out }),
    drained: () => buffered === 0 && performance.now() - lastPlay > REPORT_LAG_MS,
    close() {
      closed = true
      stopMic()
      playback?.disconnect()
      playback = null
    },
  }
}
