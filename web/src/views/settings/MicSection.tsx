import { useEffect, useState } from 'react'
import { api } from '../../api'
import { t } from '../../i18n'
import { autoOn, GAIN_MAX_DB, rememberGains, slid, type Mic, type MicPatch } from '../../mic'
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

export function MicSection() {
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
