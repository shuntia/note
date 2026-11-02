import { useEffect, useRef, useState } from 'react'
import { api } from '../../api'
import { desktop, type MicAccess } from '../../desktop'
import { t } from '../../i18n'
import {
  autoOn,
  CALL_AUDIO,
  chooseMic,
  chosenMic,
  GAIN_MAX_DB,
  micInputs,
  openMic,
  rememberGains,
  slid,
  type Mic,
  type MicInput,
  type MicPatch,
} from '../../mic'
import { Switch } from '../../ui/rows'

const SAVE_WAIT_MS = 300

// Holds back a dragged slider's saves until it rests; a switch to auto drops any held one.
export function micSaver(put: (client: string, patch: MicPatch) => Promise<Mic>, wait = SAVE_WAIT_MS) {
  const held = new Map<string, ReturnType<typeof setTimeout>>()
  const drop = (client: string) => {
    clearTimeout(held.get(client))
    held.delete(client)
  }
  return {
    gain(client: string, db: number, failed: () => void) {
      drop(client)
      held.set(
        client,
        setTimeout(() => {
          held.delete(client)
          put(client, { gain_db: db }).catch(failed)
        }, wait),
      )
    },
    auto(client: string): Promise<Mic> {
      drop(client)
      return put(client, { auto: true })
    },
    stop() {
      for (const client of [...held.keys()]) drop(client)
    },
  }
}

const micName = (m: Mic) => (m.kind === 'element' ? t('settings.mic.element') : m.label || t(`settings.mic.${m.kind}`))

function Glyph({ kind }: { kind: Mic['kind'] }) {
  return (
    <svg className="set-mic-glyph" viewBox="0 0 24 24" aria-hidden="true">
      {kind === 'element' ? (
        <rect x="7" y="3" width="10" height="18" rx="2.5" />
      ) : kind === 'desktop' ? (
        <path d="M3 5h18v11H3zM9 20h6M12 16v4" />
      ) : (
        <>
          <circle cx="12" cy="12" r="9" />
          <path d="M3 12h18M12 3c3 3.5 3 14.5 0 18M12 3c-3 3.5-3 14.5 0 18" />
        </>
      )}
    </svg>
  )
}

export function MicRows({
  mics,
  onSlide,
  onAuto,
}: {
  mics: Mic[]
  onSlide: (client: string, db: number) => void
  onAuto: (client: string) => void
}) {
  return (
    <div className="set-mics">
      {mics.map((m) => (
        <div key={m.client} className="set-row set-mic" data-auto={m.auto}>
          <Glyph kind={m.kind} />
          <span className="set-row-body">
            <span className="set-label">{micName(m)}</span>
            <input
              type="range"
              min={0}
              max={GAIN_MAX_DB}
              step={1}
              value={m.gain_db}
              aria-label={`${micName(m)} ${t('settings.mic.gain')}`}
              aria-valuetext={`+${Math.round(m.gain_db)} dB`}
              onChange={(e) => onSlide(m.client, Number(e.target.value))}
            />
          </span>
          <Switch label={`${micName(m)} ${t('settings.mic.auto')}`} on={m.auto} onToggle={() => !m.auto && onAuto(m.client)} />
        </div>
      ))}
    </div>
  )
}

type Probe = 'opening' | 'live' | 'blocked' | 'failed'

const isRefusal = (e: unknown) => e instanceof Error && (e.name === 'NotAllowedError' || e.name === 'SecurityError')

// This machine's input: which one calls use, and how loud it is right now.
function MicInputRow() {
  const [inputs, setInputs] = useState<MicInput[]>([])
  const [chosen, setChosen] = useState(chosenMic)
  const [probe, setProbe] = useState<Probe>('opening')
  const [access, setAccess] = useState<MicAccess | null>(null)
  const meter = useRef<HTMLSpanElement>(null)

  useEffect(() => {
    const list = () =>
      void navigator.mediaDevices
        ?.enumerateDevices()
        .then((all) => setInputs(micInputs(all)))
        .catch(() => {})
    list()
    navigator.mediaDevices?.addEventListener?.('devicechange', list)
    return () => navigator.mediaDevices?.removeEventListener?.('devicechange', list)
  }, [probe])

  useEffect(() => {
    let left = false
    let stream: MediaStream | null = null
    let ctx: AudioContext | null = null
    let frame = 0
    setProbe('opening')
    openMic(CALL_AUDIO, undefined, chosen).then(
      (opened) => {
        stream = opened
        if (left) return opened.getTracks().forEach((track) => track.stop())
        setProbe('live')
        ctx = new AudioContext()
        const analyser = ctx.createAnalyser()
        analyser.fftSize = 512
        ctx.createMediaStreamSource(opened).connect(analyser)
        const samples = new Float32Array(analyser.fftSize)
        let level = 0
        const draw = () => {
          analyser.getFloatTimeDomainData(samples)
          let sum = 0
          for (const v of samples) sum += v * v
          level = Math.max(Math.min(1, Math.sqrt(sum / samples.length) * 4), level * 0.85)
          meter.current?.style.setProperty('--level', level.toFixed(3))
          frame = requestAnimationFrame(draw)
        }
        frame = requestAnimationFrame(draw)
      },
      async (e: unknown) => {
        const os = (await desktop()?.micAccess?.().catch(() => null)) ?? null
        if (left) return
        setAccess(os)
        setProbe(isRefusal(e) || os?.status === 'denied' || os?.status === 'restricted' ? 'blocked' : 'failed')
      },
    )
    return () => {
      left = true
      cancelAnimationFrame(frame)
      stream?.getTracks().forEach((track) => track.stop())
      void ctx?.close().catch(() => {})
    }
  }, [chosen])

  const pick = (id: string) => {
    chooseMic(id || null)
    setChosen(id || null)
  }
  const value = chosen && inputs.some((d) => d.deviceId === chosen) ? chosen : ''

  return (
    <>
      <div className="set-row set-mic-input">
        <select aria-label={t('settings.mic.input')} value={value} onChange={(e) => pick(e.target.value)}>
          <option value="">{t('settings.mic.default')}</option>
          {inputs.map((d, i) => (
            <option key={d.deviceId} value={d.deviceId}>
              {d.label || `${t('settings.mic.input')} ${i + 1}`}
            </option>
          ))}
        </select>
        <span ref={meter} className="set-mic-meter" data-live={probe === 'live'} aria-hidden="true" />
      </div>
      {probe === 'blocked' && (
        <p className="set-note" role="alert">
          {t('settings.mic.blocked')}{' '}
          {access?.settings && desktop()?.micSettings && (
            <button type="button" className="link" onClick={() => desktop()?.micSettings?.()}>
              {t('settings.mic.openSettings')}
            </button>
          )}
        </p>
      )}
      {probe === 'failed' && (
        <p className="set-note" role="alert">
          {t('settings.mic.failed')}
        </p>
      )}
    </>
  )
}

export function MicSection() {
  return (
    <>
      <MicInputRow />
      <MicGains />
    </>
  )
}

function MicGains() {
  const [mics, setMics] = useState<Mic[] | null>(null)
  const [saver] = useState(() => micSaver(api.voiceMicSet))
  const load = () => {
    api.voiceMics().then(
      (r) => {
        rememberGains(r.mics)
        setMics(r.mics)
      },
      () => setMics([]),
    )
  }
  useEffect(load, [])
  useEffect(() => () => saver.stop(), [saver])
  if (!mics?.length) return null
  const show = (next: Mic[]) => {
    rememberGains(next)
    setMics(next)
  }
  return (
    <MicRows
      mics={mics}
      onSlide={(client, db) => {
        show(slid(mics, client, db))
        saver.gain(client, db, load)
      }}
      onAuto={(client) => {
        show(autoOn(mics, client))
        saver.auto(client).then((row) => {
          rememberGains([row])
          setMics((cur) => cur && cur.map((m) => (m.client === row.client ? row : m)))
        }, load)
      }}
    />
  )
}
