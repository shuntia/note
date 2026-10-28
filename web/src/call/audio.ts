import { realDevice, type MicTrack } from '../mic'
import captureUrl from './capture.worklet.ts?worker&url'
import playbackUrl from './playback.worklet.ts?worker&url'
import { rms16 } from './pcm'
import { takeAudioContext } from './prime'
import type { AudioIo } from './session'

const REPORT_LAG_MS = 100
const ANALYSER_SIZE = 512

function rmsOf(samples: Float32Array): number {
  let sum = 0
  for (const s of samples) sum += s * s
  return samples.length ? Math.sqrt(sum / samples.length) : 0
}

export function webAudio(): AudioIo {
  const ctx = takeAudioContext()
  let stream: MediaStream | null = null
  let source: MediaStreamAudioSourceNode | null = null
  let analyser: AnalyserNode | null = null
  let samples: Float32Array<ArrayBuffer> | null = null
  let closed = false
  let capture: AudioWorkletNode | null = null
  let playback: AudioWorkletNode | null = null
  let watchDevice: (() => void) | null = null
  let mic = 0
  let out = 0
  let buffered = 0
  let lastPlay = -Infinity
  const stopMic = () => {
    if (watchDevice) navigator.mediaDevices.removeEventListener?.('devicechange', watchDevice)
    watchDevice = null
    stream?.getTracks().forEach((track) => track.stop())
    stream = null
    source?.disconnect()
    source = null
    analyser = null
    samples = null
    capture?.disconnect()
    capture = null
    mic = 0
  }
  const input = () => {
    if (!analyser || !samples) return 0
    analyser.getFloatTimeDomainData(samples)
    return rmsOf(samples)
  }
  return {
    async startMic(onFrame, onDevice) {
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
      analyser = ctx.createAnalyser()
      analyser.fftSize = ANALYSER_SIZE
      samples = new Float32Array(analyser.fftSize)
      source.connect(analyser)
      const track = stream.getAudioTracks()[0]
      if (!track || !onDevice) return
      const seen = async (): Promise<MicTrack> => {
        const { deviceId, groupId } = track.getSettings()
        const inputs = (await navigator.mediaDevices.enumerateDevices().catch(() => [])).filter((d) => d.kind === 'audioinput')
        return realDevice({ deviceId, groupId, label: track.label }, inputs)
      }
      let last = await seen()
      ensureOpen()
      onDevice(last)
      watchDevice = () => {
        void seen().then((now) => {
          if (closed || (now.deviceId === last.deviceId && now.label === last.label)) return
          last = now
          onDevice(now)
        })
      }
      navigator.mediaDevices.addEventListener?.('devicechange', watchDevice)
    },
    stopMic,
    play(pcm, rate) {
      lastPlay = performance.now()
      playback?.port.postMessage({ pcm: pcm.buffer, rate }, [pcm.buffer as ArrayBuffer])
    },
    flush() {
      playback?.port.postMessage({ flush: true })
    },
    levels: () => ({ mic, out, input: input() }),
    drained: () => buffered === 0 && performance.now() - lastPlay > REPORT_LAG_MS,
    close() {
      closed = true
      stopMic()
      playback?.disconnect()
      playback = null
    },
  }
}
