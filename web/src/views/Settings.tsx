import QRCode from 'qrcode'
import { useEffect, useMemo, useRef, useState, type FormEvent, type ReactNode } from 'react'
import { api, ApiError, security } from '../api'
import type { ToastAction, ViewProps } from '../app'
import { adoptLanguage, t, type Key } from '../i18n'
import * as format from '../i18n/format'
import { Overflow } from '../overflow'
import { prefsFrom, writePrefs } from '../prefs'
import { disablePush, enablePush, pushState } from '../push'
import { eventLabel } from '../receipts'
import type { CounterMode } from '../session'
import { paletteAt, solarAltitude, sunTimes } from '../sky'
import '../styles/settings.css'
import {
  applyTheme,
  currentPlace,
  paintSky,
  savePlace,
  saveTheme,
  storedPlace,
  storedTheme,
  type ThemeChoice,
} from '../theme'
import type {
  BuiltPrompt,
  PromptKind,
  Language,
  Me,
  Passkey,
  RingFor,
  ScheduleRow,
  SecurityState,
  Settings as UserSettings,
  Share,
  SharePatch,
  ShareMessage,
  ShareScope,
  ShareThread,
  ShareVisit,
  Token,
  TokenCreated,
  TotpEnrolment,
  VoiceChoice,
} from '../types'
import { previewPlayer } from '../voicePreview'
import { VoiceSheet } from '../voiceSheet'
import { createCredential, webauthnSupported, type RegistrationJSON } from '../webauthn'
import { deviceZone, knownDeviceZone } from '../zone'

type Notify = (msg: string, action?: ToastAction) => void

const EDITABLE = [
  'display_name',
  'timezone',
  'nightly_time',
  'close_day_time',
  'morning_until',
  'template',
  'triggers_per_day',
  'pomodoro_work_min',
  'pomodoro_break_min',
  'idle_nudge_min',
] as const

const MAX_TRIGGERS_PER_DAY = 20
const MXID_EXAMPLE = '@you:server'
const WORK_MIN = { min: 5, max: 120 }
const BREAK_MIN = { min: 1, max: 60 }
const IDLE_MIN = { min: 0, max: 240 }

type Draft = Pick<UserSettings, (typeof EDITABLE)[number]>
type Choices = Pick<UserSettings, 'templates' | 'timezones'>
type Loaded = {
  choices: Choices
  baseline: Draft
  draft: Draft
  rows: ScheduleRow[]
  arc: boolean
  counter: CounterMode
  nightly: boolean
  checkins: boolean
  pomodoro: boolean
  endNotify: boolean
  zoneAuto: boolean
  voiceEnabled: boolean
  voiceLink: UserSettings['voice_link']
  ringFor: RingFor
  voice: string
  cue: boolean
  matrixEnabled: boolean
  matrixSend: boolean
  matrixPing: boolean
  language: Language
}
type Save = { row: string; kind: 'busy' | 'saved' | 'failed'; message?: string } | null

const LANGUAGES: { id: Language; key: Key }[] = [
  { id: '', key: 'settings.language.auto' },
  { id: 'en', key: 'settings.language.en' },
  { id: 'ja', key: 'settings.language.ja' },
]

const RING_CHOICES: { id: RingFor; key: Key }[] = [
  { id: 'urgent', key: 'settings.ring.urgent' },
  { id: 'checkins', key: 'settings.ring.checkins' },
  { id: 'never', key: 'settings.ring.never' },
]

export function RingChoices({
  value,
  disabled,
  onPick,
}: {
  value: RingFor
  disabled?: boolean
  onPick: (choice: RingFor) => void
}) {
  return (
    <div className="seg" role="group" aria-label={t('settings.ring.label')}>
      {RING_CHOICES.map((c) => (
        <button
          key={c.id}
          type="button"
          aria-pressed={value === c.id}
          disabled={disabled}
          onClick={() => onPick(c.id)}
        >
          {t(c.key)}
        </button>
      ))}
    </div>
  )
}

const THEMES: { id: ThemeChoice; key: Key }[] = [
  { id: 'system', key: 'settings.theme.system' },
  { id: 'light', key: 'settings.theme.light' },
  { id: 'dark', key: 'settings.theme.dark' },
  { id: 'sky', key: 'settings.theme.sky' },
]

// Today's sky from midnight to midnight at half-hour stops, with a mark at now.
function SkyStrip() {
  const [minute, setMinute] = useState(() => Math.floor(Date.now() / 60_000))
  useEffect(() => {
    const id = window.setInterval(() => setMinute(Math.floor(Date.now() / 60_000)), 60_000)
    return () => window.clearInterval(id)
  }, [])
  const { times, stops, mins } = useMemo(() => {
    const zone = deviceZone()
    const place = currentPlace()
    const now = new Date(minute * 60_000)
    const stops = Array.from({ length: 49 }, (_, i) => {
      const at = new Date(now)
      at.setHours(0, i * 30, 0, 0)
      const { tokens } = paletteAt(solarAltitude(at, place.lat, place.lon))
      return `${tokens['sky-mid']} ${(i / 48) * 100}%`
    })
    return { times: sunTimes(now, place, zone), stops, mins: now.getHours() * 60 + now.getMinutes() }
  }, [minute])
  return (
    <div className="sky-strip-wrap">
      <div className="sky-strip" style={{ background: `linear-gradient(90deg, ${stops.join(', ')})` }}>
        <i style={{ left: `${(mins / 1440) * 100}%` }} />
      </div>
      <div className="sky-ticks">
        <span>0:00</span>
        <span>{times.rise ? t('settings.sky.rise', { time: times.rise }) : t('settings.sky.noSunrise')}</span>
        <span>{times.set ? t('settings.sky.set', { time: times.set }) : t('settings.sky.noSunset')}</span>
        <span>24:00</span>
      </div>
    </div>
  )
}

const COUNTERS: { id: CounterMode; key: Key }[] = [
  { id: 'remaining', key: 'settings.counter.remaining' },
  { id: 'elapsed', key: 'settings.counter.elapsed' },
]

function draftOf(s: UserSettings): Draft {
  return {
    display_name: s.display_name,
    timezone: s.timezone,
    nightly_time: s.nightly_time,
    close_day_time: s.close_day_time,
    morning_until: s.morning_until,
    template: s.template,
    triggers_per_day: s.triggers_per_day,
    pomodoro_work_min: s.pomodoro_work_min,
    pomodoro_break_min: s.pomodoro_break_min,
    idle_nudge_min: s.idle_nudge_min,
  }
}

const templateChoices = (loaded: { choices: Choices; draft: Draft }) =>
  loaded.choices.templates.includes(loaded.draft.template)
    ? loaded.choices.templates
    : [loaded.draft.template, ...loaded.choices.templates]

// A zone reads as the city it names; the region is the same for every choice nearby.
const zoneCity = (tz: string) => tz.split('/').pop()?.replace(/_/g, ' ') ?? tz

// Only a real slide window is worth a line; everything else the day already shows.
function slideWord(row: ScheduleRow): string | null {
  if (row.entry === 'block' || row.flexibility === 'fixed' || row.slide_window_min <= 0) return null
  const min = row.slide_window_min
  const span = min % 60 === 0 ? t('time.hours', { n: min / 60 }) : t('time.minutes', { n: min })
  return `± ${span}`
}

function Group({ head, children }: { head: string; children: ReactNode }) {
  return (
    <section className="set-group">
      <h2 className="set-group-head">{head}</h2>
      {children}
    </section>
  )
}

function Status({ save, row }: { save: Save; row: string }) {
  if (!save || save.row !== row || save.kind === 'busy') return null
  if (save.kind === 'saved')
    return (
      <span className="pane-status ok" role="status">
        {save.message ?? t('settings.saved')}
      </span>
    )
  return (
    <span className="pane-status bad" role="alert">
      {save.message}
    </span>
  )
}

function Switch({
  label,
  on,
  disabled,
  onToggle,
}: {
  label: string
  on: boolean
  disabled?: boolean
  onToggle?: () => void
}) {
  return (
    <button
      type="button"
      role="switch"
      className="sw"
      aria-checked={on}
      aria-label={label}
      disabled={disabled}
      onClick={onToggle}
    />
  )
}

function FoldRow({
  label,
  value,
  open,
  onToggle,
  children,
}: {
  label: string
  value?: string
  open: boolean
  onToggle: () => void
  children: ReactNode
}) {
  return (
    <div className="set-fold">
      <button className="set-row set-open" aria-expanded={open} onClick={onToggle}>
        <span className="set-row-body">
          <span className="set-label">{label}</span>
        </span>
        {value && <span className="set-value">{value}</span>}
        <svg className="set-chev" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M9 6l6 6-6 6" />
        </svg>
      </button>
      {children}
    </div>
  )
}

export function Settings({
  me,
  notify,
  onChanged,
  onSignedOut,
  openAdmin,
}: ViewProps & { me: Me; onSignedOut: () => void; openAdmin: () => void }) {
  const [state, setState] = useState<Loaded | 'error' | undefined>(undefined)
  const [save, setSave] = useState<Save>(null)
  const [open, setOpen] = useState<string | null>(null)
  const [theme, setTheme] = useState<ThemeChoice>(storedTheme)
  const [place, setPlace] = useState(storedPlace)
  const [mxid, setMxid] = useState('')

  const load = () => {
    setState(undefined)
    setSave(null)
    api
      .settings()
      .then((s) => {
        setState({
          choices: { templates: s.templates, timezones: s.timezones },
          baseline: draftOf(s),
          draft: draftOf(s),
          rows: s.schedule,
          arc: s.show_arc_between_sessions,
          counter: s.counter,
          nightly: s.nightly_enabled,
          checkins: s.checkins_enabled,
          pomodoro: s.pomodoro_enabled,
          endNotify: s.session_end_notify,
          zoneAuto: s.timezone_auto,
          voiceEnabled: s.voice_enabled,
          voiceLink: s.voice_link,
          ringFor: s.ring_for,
          voice: s.voice_voice,
          cue: s.voice_cue,
          matrixEnabled: s.matrix_enabled,
          matrixSend: s.matrix_send,
          matrixPing: s.matrix_ping,
          language: s.language,
        })
      })
      .catch(() => setState('error'))
  }
  useEffect(load, [])

  const loaded = state !== undefined && state !== 'error' ? state : null

  const fold = (id: string) => () => setOpen((o) => (o === id ? null : id))

  const edit = <K extends keyof Draft>(key: K, value: Draft[K]) =>
    setState((s) => (s && s !== 'error' ? { ...s, draft: { ...s.draft, [key]: value } } : s))

  const failure = (err: unknown) =>
    err instanceof ApiError && (err.status === 400 || err.status === 422)
      ? err.message
      : t('settings.saveFailed')

  const callFailure = (err: unknown) =>
    err instanceof ApiError && [400, 409, 422, 503].includes(err.status)
      ? err.message
      : t('settings.notThrough')

  // Only the fields that moved travel, so an edit left open in another row is never
  // written by someone else's save. `override` carries a value whose state update
  // has not landed yet, for controls that save on change.
  const commit = async (row: string, override?: Partial<Draft>) => {
    if (!loaded) return
    const cleaned: Draft = {
      ...loaded.draft,
      ...override,
      display_name: loaded.draft.display_name.trim(),
    }
    const patch: Partial<Draft> = {}
    // Draft mixes text and numbers, so each field travels as its own key.
    for (const key of EDITABLE) {
      if (cleaned[key] !== loaded.baseline[key]) Object.assign(patch, { [key]: cleaned[key] })
    }
    if (Object.keys(patch).length === 0) return
    setSave({ row, kind: 'busy' })
    try {
      const saved = await api.saveSettings(patch)
      setState((s) =>
        s && s !== 'error' ? { ...s, baseline: cleaned, draft: cleaned, rows: saved.schedule } : s,
      )
      writePrefs(prefsFrom(saved))
      setSave({ row, kind: 'saved' })
    } catch (err) {
      setSave({ row, kind: 'failed', message: failure(err) })
    }
  }

  const commitHome = async (
    row: string,
    patch: { show_arc_between_sessions?: boolean; counter?: CounterMode },
  ) => {
    setSave({ row, kind: 'busy' })
    try {
      const saved = await api.saveSettings(patch)
      setState((s) =>
        s && s !== 'error'
          ? { ...s, arc: saved.show_arc_between_sessions, counter: saved.counter }
          : s,
      )
      writePrefs(prefsFrom(saved))
      setSave({ row, kind: 'saved' })
    } catch (err) {
      setSave({ row, kind: 'failed', message: failure(err) })
    }
  }

  const chooseLanguage = async (language: Language) => {
    setSave({ row: 'language', kind: 'busy' })
    try {
      const saved = await api.saveSettings({ language })
      if (adoptLanguage(saved.language)) {
        location.reload()
        return
      }
      setState((s) => (s && s !== 'error' ? { ...s, language: saved.language } : s))
      setSave({ row: 'language', kind: 'saved' })
    } catch (err) {
      setSave({ row: 'language', kind: 'failed', message: failure(err) })
    }
  }

  const commitFeature = async (
    row: string,
    patch: {
      nightly_enabled?: boolean
      checkins_enabled?: boolean
      pomodoro_enabled?: boolean
      session_end_notify?: boolean
    },
  ) => {
    setSave({ row, kind: 'busy' })
    try {
      const saved = await api.saveSettings(patch)
      setState((s) =>
        s && s !== 'error'
          ? {
              ...s,
              nightly: saved.nightly_enabled,
              checkins: saved.checkins_enabled,
              pomodoro: saved.pomodoro_enabled,
              endNotify: saved.session_end_notify,
            }
          : s,
      )
      setSave({ row, kind: 'saved' })
    } catch (err) {
      setSave({ row, kind: 'failed', message: failure(err) })
    }
  }

  const linkVoice = async () => {
    setSave({ row: 'matrix', kind: 'busy' })
    try {
      const got = await api.voiceLink(mxid.trim())
      setState((s) =>
        s && s !== 'error' ? { ...s, voiceLink: { mxid: got.mxid, state: 'invited' } } : s,
      )
      setMxid('')
      setSave(null)
    } catch (err) {
      setSave({ row: 'matrix', kind: 'failed', message: callFailure(err) })
    }
  }

  const unlinkVoice = async () => {
    setSave({ row: 'matrix', kind: 'busy' })
    try {
      await api.voiceUnlink()
      setState((s) => (s && s !== 'error' ? { ...s, voiceLink: null } : s))
      setSave({ row: 'matrix', kind: 'saved' })
    } catch (err) {
      setSave({ row: 'matrix', kind: 'failed', message: callFailure(err) })
    }
  }

  const ringNow = async () => {
    setSave({ row: 'calls', kind: 'busy' })
    try {
      await api.voiceTest()
      setSave({ row: 'calls', kind: 'saved', message: t('settings.ringing') })
    } catch (err) {
      setSave({ row: 'calls', kind: 'failed', message: callFailure(err) })
    }
  }

  const saveMatrix = async (patch: { matrix_send?: boolean; matrix_ping?: boolean }) => {
    setSave({ row: 'matrix', kind: 'busy' })
    try {
      const saved = await api.saveSettings(patch)
      setState((s) =>
        s && s !== 'error' ? { ...s, matrixSend: saved.matrix_send, matrixPing: saved.matrix_ping } : s,
      )
      setSave({ row: 'matrix', kind: 'saved' })
    } catch (err) {
      setSave({ row: 'matrix', kind: 'failed', message: failure(err) })
    }
  }

  const saveRingFor = async (ringFor: RingFor) => {
    setSave({ row: 'calls', kind: 'busy' })
    try {
      const saved = await api.saveSettings({ ring_for: ringFor })
      setState((s) => (s && s !== 'error' ? { ...s, ringFor: saved.ring_for } : s))
      setSave({ row: 'calls', kind: 'saved' })
    } catch (err) {
      setSave({ row: 'calls', kind: 'failed', message: failure(err) })
    }
  }

  const [voices, setVoices] = useState<VoiceChoice[] | null>(null)
  const [preview] = useState(() => previewPlayer())
  useEffect(() => () => preview.stop(), [preview])
  const callsLinked = open === 'calls' && loaded?.voiceLink?.state === 'linked'
  useEffect(() => {
    if (!callsLinked) return
    let live = true
    api
      .voiceVoices()
      .then(({ voices }) => live && setVoices(voices))
      .catch(() => undefined)
    return () => {
      live = false
    }
  }, [callsLinked])

  const [choosingVoice, setChoosingVoice] = useState(false)
  const pickVoice = async (voice: string) => {
    const before = loaded?.voice ?? ''
    setState((s) => (s && s !== 'error' ? { ...s, voice } : s))
    try {
      await api.saveSettings({ voice_voice: voice })
    } catch (err) {
      setState((s) => (s && s !== 'error' && s.voice === voice ? { ...s, voice: before } : s))
      setSave({ row: 'calls', kind: 'failed', message: failure(err) })
    }
  }

  const saveCue = async (cue: boolean) => {
    setSave({ row: 'calls', kind: 'busy' })
    try {
      const saved = await api.saveSettings({ voice_cue: cue })
      setState((s) => (s && s !== 'error' ? { ...s, cue: saved.voice_cue } : s))
      setSave(null)
    } catch (err) {
      setSave({ row: 'calls', kind: 'failed', message: failure(err) })
    }
  }

  // The invite is accepted over in Element, so the row watches for it.
  const invited = loaded?.voiceLink?.state === 'invited'
  useEffect(() => {
    if (!invited) return
    const id = window.setInterval(() => {
      void api
        .settings()
        .then((s) => {
          const next = s.voice_link
          setState((prev) =>
            prev &&
            prev !== 'error' &&
            (prev.voiceLink?.mxid !== next?.mxid || prev.voiceLink?.state !== next?.state)
              ? { ...prev, voiceLink: next }
              : prev,
          )
        })
        .catch(() => undefined)
    }, 3000)
    return () => window.clearInterval(id)
  }, [invited])

  const sendTest = async () => {
    setSave({ row: 'test', kind: 'busy' })
    try {
      const { via } = await api.notifyTest()
      setSave({ row: 'test', kind: 'saved', message: t('settings.test.sent', { via }) })
    } catch {
      setSave({ row: 'test', kind: 'failed', message: t('settings.test.failed') })
    }
  }

  const toggleAlert = async (row: ScheduleRow) => {
    const next = !row.alert
    const setAlert = (value: boolean) =>
      setState((s) =>
        s && s !== 'error'
          ? { ...s, rows: s.rows.map((r) => (r.index === row.index ? { ...r, alert: value } : r)) }
          : s,
      )
    setSave({ row: 'schedule', kind: 'busy' })
    setAlert(next)
    try {
      const saved = await api.saveSettings({}, [{ index: row.index, alert: next }])
      setState((s) => (s && s !== 'error' ? { ...s, rows: saved.schedule } : s))
      setSave({ row: 'schedule', kind: 'saved' })
    } catch (err) {
      setAlert(row.alert)
      setSave({ row: 'schedule', kind: 'failed', message: failure(err) })
    }
  }

  const chooseTheme = (choice: ThemeChoice) => {
    setTheme(choice)
    applyTheme(choice)
    saveTheme(choice)
  }

  // Turning the switch on moves the day to this device at once rather than on the
  // next focus.
  const commitZoneAuto = async (on: boolean) => {
    if (!loaded) return
    const device = deviceZone()
    const follow = on && knownDeviceZone(device, loaded.choices.timezones)
    setSave({ row: 'timezone', kind: 'busy' })
    try {
      const saved = await api.saveSettings(
        follow ? { timezone_auto: on, timezone: device } : { timezone_auto: on },
      )
      setState((s) =>
        s && s !== 'error'
          ? {
              ...s,
              zoneAuto: saved.timezone_auto,
              baseline: { ...s.baseline, timezone: saved.timezone },
              draft: { ...s.draft, timezone: saved.timezone },
              rows: saved.schedule,
            }
          : s,
      )
      setSave({ row: 'timezone', kind: 'saved' })
      onChanged()
    } catch (err) {
      setSave({ row: 'timezone', kind: 'failed', message: failure(err) })
    }
  }

  const locate = () => {
    if (!navigator.geolocation) return notify(t('settings.locateFailed'))
    navigator.geolocation.getCurrentPosition(
      (pos) => {
        const p = {
          lat: Math.round(pos.coords.latitude * 10) / 10,
          lon: Math.round(pos.coords.longitude * 10) / 10,
        }
        savePlace(p)
        setPlace(p)
        paintSky()
      },
      () => notify(t('settings.locateFailed')),
      { maximumAge: 3_600_000, timeout: 10_000 },
    )
  }
  const forget = () => {
    savePlace(null)
    setPlace(null)
    paintSky()
  }

  const commitOn = (row: string) => ({
    onBlur: () => void commit(row),
    onKeyDown: (e: { key: string; currentTarget: HTMLInputElement }) => {
      if (e.key === 'Enter') e.currentTarget.blur()
    },
  })

  const busy = save?.kind === 'busy'
  const themeLabel = t(THEMES.find((x) => x.id === theme)?.key ?? 'settings.theme.system')
  const counterLabel = t(COUNTERS.find((c) => c.id === loaded?.counter)?.key ?? 'settings.counter.remaining')

  return (
    <div className="settings">
      {state === 'error' && (
        <p className="set-sub">
          {t('settings.loadFailed')}{' '}
          <button className="set-link" onClick={load}>
            {t('common.retry')}
          </button>
        </p>
      )}

      {loaded && (
        <Group head={t('settings.sessions')}>
          <FoldRow
            label={t('settings.pomodoro')}
            value={
              loaded.pomodoro
                ? t('settings.pomodoro.value', {
                    work: loaded.draft.pomodoro_work_min,
                    rest: loaded.draft.pomodoro_break_min,
                  })
                : t('settings.off')
            }
            open={open === 'pomodoro'}
            onToggle={fold('pomodoro')}
          >
            {open === 'pomodoro' && (
              <div className="set-fold-body">
                <span className="set-sub">{t('settings.pomodoro.sub')}</span>
                <div className="set-row">
                  <span className="set-row-body">
                    <span className="set-label">{t('settings.pomodoro.rounds')}</span>
                  </span>
                  <Switch
                    label={t('settings.pomodoro')}
                    on={loaded.pomodoro}
                    disabled={busy}
                    onToggle={() =>
                      void commitFeature('pomodoro', { pomodoro_enabled: !loaded.pomodoro })
                    }
                  />
                </div>
                <label className="set-sub" htmlFor="pomodoro-work">
                  {t('settings.pomodoro.work')}
                </label>
                <input
                  id="pomodoro-work"
                  type="number"
                  min={WORK_MIN.min}
                  max={WORK_MIN.max}
                  value={loaded.draft.pomodoro_work_min}
                  onChange={(e) => edit('pomodoro_work_min', Math.round(Number(e.target.value) || 0))}
                  {...commitOn('pomodoro')}
                />
                <label className="set-sub" htmlFor="pomodoro-break">
                  {t('settings.pomodoro.break')}
                </label>
                <input
                  id="pomodoro-break"
                  type="number"
                  min={BREAK_MIN.min}
                  max={BREAK_MIN.max}
                  value={loaded.draft.pomodoro_break_min}
                  onChange={(e) => edit('pomodoro_break_min', Math.round(Number(e.target.value) || 0))}
                  {...commitOn('pomodoro')}
                />
                <Status save={save} row="pomodoro" />
              </div>
            )}
          </FoldRow>
          <div className="set-row">
            <span className="set-row-body">
              <span className="set-label">{t('settings.notifyEnd')}</span>
            </span>
            <Status save={save} row="session_end_notify" />
            <Switch
              label={t('settings.notifyEnd')}
              on={loaded.endNotify}
              disabled={busy}
              onToggle={() =>
                void commitFeature('session_end_notify', { session_end_notify: !loaded.endNotify })
              }
            />
          </div>
          <FoldRow
            label={t('settings.counter')}
            value={counterLabel}
            open={open === 'counter'}
            onToggle={fold('counter')}
          >
            {open === 'counter' && (
              <div className="set-fold-body">
                <div className="seg" role="group" aria-label={t('settings.counter')}>
                  {COUNTERS.map((c) => (
                    <button
                      key={c.id}
                      type="button"
                      aria-pressed={loaded.counter === c.id}
                      onClick={() => void commitHome('counter', { counter: c.id })}
                    >
                      {t(c.key)}
                    </button>
                  ))}
                </div>
                <Status save={save} row="counter" />
              </div>
            )}
          </FoldRow>
          <div className="set-row">
            <span className="set-row-body">
              <span className="set-label">{t('settings.arc')}</span>
            </span>
            <Status save={save} row="arc" />
            <Switch
              label={t('settings.arc')}
              on={loaded.arc}
              disabled={busy}
              onToggle={() => void commitHome('arc', { show_arc_between_sessions: !loaded.arc })}
            />
          </div>
        </Group>
      )}

      {loaded && (
        <Group head={t('settings.day')}>
          <FoldRow
            label={t('settings.schedule')}
            value={loaded.draft.template}
            open={open === 'schedule'}
            onToggle={fold('schedule')}
          >
            {open === 'schedule' && (
              <div className="set-fold-body">
                {templateChoices(loaded).length > 1 && (
                  <select
                    aria-label={t('settings.schedule.shape')}
                    value={loaded.draft.template}
                    onChange={(e) => {
                      edit('template', e.target.value)
                      void commit('schedule', { template: e.target.value })
                    }}
                  >
                    {templateChoices(loaded).map((name) => (
                      <option key={name} value={name}>
                        {name}
                      </option>
                    ))}
                  </select>
                )}
                <ScheduleList rows={loaded.rows} busy={busy} toggle={toggleAlert} />
                <Status save={save} row="schedule" />
              </div>
            )}
          </FoldRow>
          <FoldRow
            label={t('settings.nightly')}
            value={loaded.nightly ? t('settings.nightly.on', { time: loaded.draft.nightly_time }) : t('settings.off')}
            open={open === 'nightly'}
            onToggle={fold('nightly')}
          >
            {open === 'nightly' && (
              <div className="set-fold-body">
                <div className="set-row">
                  <span className="set-row-body">
                    <span className="set-label">{t('settings.nightly.sub')}</span>
                  </span>
                  <Status save={save} row="nightly_enabled" />
                  <Switch
                    label={t('settings.nightly')}
                    on={loaded.nightly}
                    disabled={busy}
                    onToggle={() =>
                      void commitFeature('nightly_enabled', { nightly_enabled: !loaded.nightly })
                    }
                  />
                </div>
                <label className="set-sub" htmlFor="nightly-time">
                  {t('settings.nightly.landsAt')}
                </label>
                <input
                  id="nightly-time"
                  type="time"
                  value={loaded.draft.nightly_time}
                  onChange={(e) => edit('nightly_time', e.target.value)}
                  {...commitOn('nightly')}
                />
                <Status save={save} row="nightly" />
              </div>
            )}
          </FoldRow>
          <FoldRow
            label={t('settings.closeDay')}
            value={loaded.draft.close_day_time || t('settings.off')}
            open={open === 'close_day'}
            onToggle={fold('close_day')}
          >
            {open === 'close_day' && (
              <div className="set-fold-body">
                <span className="set-sub">{t('settings.closeDay.sub')}</span>
                <input
                  type="time"
                  aria-label={t('settings.closeDay')}
                  value={loaded.draft.close_day_time}
                  onChange={(e) => edit('close_day_time', e.target.value)}
                  {...commitOn('close_day')}
                />
                <Status save={save} row="close_day" />
              </div>
            )}
          </FoldRow>
          <FoldRow
            label={t('settings.timezone')}
            value={zoneCity(loaded.draft.timezone)}
            open={open === 'timezone'}
            onToggle={fold('timezone')}
          >
            {open === 'timezone' && (
              <div className="set-fold-body">
                <div className="set-row">
                  <span className="set-row-body">
                    <span className="set-label">{t('settings.followDevice')}</span>
                  </span>
                  <Switch
                    label={t('settings.followDevice')}
                    on={loaded.zoneAuto}
                    disabled={busy}
                    onToggle={() => void commitZoneAuto(!loaded.zoneAuto)}
                  />
                </div>
                {loaded.zoneAuto ? (
                  <span className="set-zone">
                    <input aria-label={t('settings.timezone')} disabled value={loaded.draft.timezone} />
                    {loaded.draft.timezone === deviceZone() && <span className="set-hint">{t('settings.detected')}</span>}
                  </span>
                ) : (
                  <input
                    list="tz-list"
                    aria-label={t('settings.timezone')}
                    spellCheck={false}
                    autoCapitalize="none"
                    value={loaded.draft.timezone}
                    onChange={(e) => edit('timezone', e.target.value)}
                    {...commitOn('timezone')}
                  />
                )}
                <datalist id="tz-list">
                  {loaded.choices.timezones.map((tz) => (
                    <option key={tz} value={tz} />
                  ))}
                </datalist>
                <Status save={save} row="timezone" />
              </div>
            )}
          </FoldRow>
        </Group>
      )}

      <Group head={t('settings.notifications')}>
        <PushRow notify={notify} />
        {loaded && (
          <div className="set-row">
            <span className="set-row-body">
              <span className="set-label">{t('settings.checkins')}</span>
              <span className="set-sub">{t('settings.checkins.sub')}</span>
            </span>
            <Status save={save} row="checkins_enabled" />
            <Switch
              label={t('settings.checkins')}
              on={loaded.checkins}
              disabled={busy}
              onToggle={() =>
                void commitFeature('checkins_enabled', { checkins_enabled: !loaded.checkins })
              }
            />
          </div>
        )}
        {loaded && (
          <FoldRow
            label={t('settings.triggers')}
            value={t('settings.triggers.value', { n: loaded.draft.triggers_per_day })}
            open={open === 'triggers'}
            onToggle={fold('triggers')}
          >
            {open === 'triggers' && (
              <div className="set-fold-body">
                <span className="set-sub">{t('settings.triggers.sub')}</span>
                <input
                  aria-label={t('settings.triggers.input')}
                  type="number"
                  min={0}
                  max={MAX_TRIGGERS_PER_DAY}
                  value={loaded.draft.triggers_per_day}
                  onChange={(e) =>
                    edit(
                      'triggers_per_day',
                      Math.min(MAX_TRIGGERS_PER_DAY, Math.max(0, Math.round(Number(e.target.value) || 0))),
                    )
                  }
                  {...commitOn('triggers')}
                />
                <Status save={save} row="triggers" />
                <div className="set-acts">
                  <button
                    type="button"
                    className="set-link"
                    disabled={busy}
                    onClick={() => void sendTest()}
                  >
                    {t('settings.test.send')}
                  </button>
                  <Status save={save} row="test" />
                </div>
              </div>
            )}
          </FoldRow>
        )}
        {loaded && (
          <FoldRow
            label={t('settings.idle')}
            value={
              loaded.draft.idle_nudge_min
                ? t('time.minutes', { n: loaded.draft.idle_nudge_min })
                : t('settings.off')
            }
            open={open === 'idle'}
            onToggle={fold('idle')}
          >
            {open === 'idle' && (
              <div className="set-fold-body">
                <input
                  aria-label={t('settings.idle.input')}
                  type="number"
                  min={IDLE_MIN.min}
                  max={IDLE_MIN.max}
                  value={loaded.draft.idle_nudge_min}
                  onChange={(e) =>
                    edit(
                      'idle_nudge_min',
                      Math.min(IDLE_MIN.max, Math.max(IDLE_MIN.min, Math.round(Number(e.target.value) || 0))),
                    )
                  }
                  {...commitOn('idle')}
                />
                <Status save={save} row="idle" />
              </div>
            )}
          </FoldRow>
        )}
      </Group>

      {loaded?.voiceEnabled && (
        <Group head={t('settings.connections')}>
          <FoldRow
            label={t('settings.matrix')}
            value={
              loaded.voiceLink?.state === 'linked'
                ? loaded.voiceLink.mxid
                : loaded.voiceLink
                  ? t('settings.matrix.invited')
                  : t('settings.matrix.notLinked')
            }
            open={open === 'matrix'}
            onToggle={fold('matrix')}
          >
            {open === 'matrix' && (
              <div className="set-fold-body">
                {loaded.voiceLink?.state === 'linked' ? (
                  <>
                    {loaded.matrixEnabled && (
                      <>
                        <div className="set-row">
                          <span className="set-row-body">
                            <span className="set-label">{t('settings.matrix.send')}</span>
                          </span>
                          <Switch
                            label={t('settings.matrix.send')}
                            on={loaded.matrixSend}
                            disabled={busy}
                            onToggle={() => void saveMatrix({ matrix_send: !loaded.matrixSend })}
                          />
                        </div>
                        {loaded.matrixSend && (
                          <div className="set-row">
                            <span className="set-row-body">
                              <span className="set-label">{t('settings.matrix.ping')}</span>
                            </span>
                            <Switch
                              label={t('settings.matrix.ping')}
                              on={loaded.matrixPing}
                              disabled={busy}
                              onToggle={() => void saveMatrix({ matrix_ping: !loaded.matrixPing })}
                            />
                          </div>
                        )}
                      </>
                    )}
                    <button
                      type="button"
                      className="btn-haze small"
                      disabled={busy}
                      onClick={() => void unlinkVoice()}
                    >
                      {t('settings.matrix.unlink')}
                    </button>
                  </>
                ) : loaded.voiceLink ? (
                  <>
                    <span className="set-sub">{t('settings.matrix.accept')}</span>
                    <button
                      type="button"
                      className="btn-haze small"
                      disabled={busy}
                      onClick={() => void unlinkVoice()}
                    >
                      {t('common.cancel')}
                    </button>
                  </>
                ) : (
                  <form
                    className="set-token-form"
                    onSubmit={(e) => {
                      e.preventDefault()
                      void linkVoice()
                    }}
                  >
                    <input
                      aria-label={t('settings.matrix.account')}
                      placeholder={MXID_EXAMPLE}
                      autoComplete="off"
                      autoCapitalize="none"
                      spellCheck={false}
                      value={mxid}
                      onChange={(e) => setMxid(e.target.value)}
                    />
                    <button type="submit" className="btn-haze small" disabled={busy || !mxid.trim()}>
                      {t('settings.matrix.link')}
                    </button>
                  </form>
                )}
                <Status save={save} row="matrix" />
              </div>
            )}
          </FoldRow>
          {loaded.voiceLink?.state === 'linked' && (
            <FoldRow
              label={t('settings.calls')}
              value={t((RING_CHOICES.find((c) => c.id === loaded.ringFor) ?? RING_CHOICES[0]).key)}
              open={open === 'calls'}
              onToggle={fold('calls')}
            >
              {open === 'calls' && (
                <div className="set-fold-body">
                  <RingChoices
                    value={loaded.ringFor}
                    disabled={busy}
                    onPick={(v) => void saveRingFor(v)}
                  />
                  {voices && voices.length > 0 && (
                    <button type="button" className="set-row set-row-button" onClick={() => setChoosingVoice(true)}>
                      <span className="set-row-body">
                        <span className="set-label">{t('settings.voice')}</span>
                      </span>
                      <span className="set-value">
                        {(voices.find((v) => v.id === loaded.voice) ?? voices[0]).label}
                      </span>
                    </button>
                  )}
                  {choosingVoice && voices && (
                    <VoiceSheet
                      voices={voices}
                      chosen={loaded.voice || voices[0].id}
                      preview={preview}
                      onPick={(v) => void pickVoice(v)}
                      onClose={() => setChoosingVoice(false)}
                    />
                  )}
                  <div className="set-row">
                    <span className="set-row-body">
                      <span className="set-label">{t('settings.sounds')}</span>
                    </span>
                    <Switch
                      label={t('settings.sounds')}
                      on={loaded.cue}
                      disabled={busy}
                      onToggle={() => void saveCue(!loaded.cue)}
                    />
                  </div>
                  <button
                    type="button"
                    className="btn-haze small"
                    disabled={busy}
                    onClick={() => void ringNow()}
                  >
                    {t('settings.ringMe')}
                  </button>
                  <Status save={save} row="calls" />
                </div>
              )}
            </FoldRow>
          )}
        </Group>
      )}

      <Group head={t('settings.you')}>
        {loaded && (
          <FoldRow
            label={t('settings.name')}
            value={loaded.draft.display_name}
            open={open === 'name'}
            onToggle={fold('name')}
          >
            {open === 'name' && (
              <div className="set-fold-body">
                <input
                  aria-label={t('settings.name')}
                  autoComplete="name"
                  value={loaded.draft.display_name}
                  onChange={(e) => edit('display_name', e.target.value)}
                  {...commitOn('name')}
                />
                <Status save={save} row="name" />
              </div>
            )}
          </FoldRow>
        )}
        <FoldRow label={t('settings.password')} open={open === 'password'} onToggle={fold('password')}>
          {open === 'password' && <PasswordSection />}
        </FoldRow>
        <FoldRow
          label={t('settings.theme')}
          value={themeLabel}
          open={open === 'theme'}
          onToggle={fold('theme')}
        >
          {open === 'theme' && (
            <div className="set-fold-body">
              <div className="seg" role="group" aria-label={t('settings.theme')}>
                {THEMES.map((x) => (
                  <button
                    key={x.id}
                    type="button"
                    aria-pressed={theme === x.id}
                    onClick={() => chooseTheme(x.id)}
                  >
                    {t(x.key)}
                  </button>
                ))}
              </div>
              {theme === 'sky' && (
                <>
                  <SkyStrip key={place ? `${place.lat},${place.lon}` : 'zone'} />
                  <p className="set-note">
                    <button type="button" className="link" onClick={place ? forget : locate}>
                      {place ? t('settings.location.forget') : t('settings.location.use')}
                    </button>
                  </p>
                </>
              )}
            </div>
          )}
        </FoldRow>
        {loaded && (
          <FoldRow
            label={t('settings.language')}
            value={t(LANGUAGES.find((x) => x.id === loaded.language)?.key ?? 'settings.language.auto')}
            open={open === 'language'}
            onToggle={fold('language')}
          >
            {open === 'language' && (
              <div className="set-fold-body">
                <div className="seg" role="group" aria-label={t('settings.language')}>
                  {LANGUAGES.map((x) => (
                    <button
                      key={x.id}
                      type="button"
                      lang={x.id || undefined}
                      aria-pressed={loaded.language === x.id}
                      disabled={busy}
                      onClick={() => void chooseLanguage(x.id)}
                    >
                      {t(x.key)}
                    </button>
                  ))}
                </div>
                <Status save={save} row="language" />
              </div>
            )}
          </FoldRow>
        )}
      </Group>

      <Group head={t('settings.advanced')}>
        {/* Stays mounted and renders nothing while closed, so an unsaved draft
            survives a detour through another row. */}
        <FoldRow label={t('settings.about')} open={open === 'about'} onToggle={fold('about')}>
          <AboutSection active={open === 'about'} />
        </FoldRow>
        <FoldRow label={t('settings.security')} open={open === 'security'} onToggle={fold('security')}>
          {open === 'security' && <SecuritySection notify={notify} />}
        </FoldRow>
        <FoldRow label={t('settings.tokens')} open={open === 'tokens'} onToggle={fold('tokens')}>
          {open === 'tokens' && <TokensSection notify={notify} />}
        </FoldRow>
        <FoldRow label={t('settings.shares')} open={open === 'shares'} onToggle={fold('shares')}>
          {open === 'shares' && <SharesSection notify={notify} />}
        </FoldRow>
        <FoldRow label={t('settings.debug')} open={open === 'debug'} onToggle={fold('debug')}>
          {open === 'debug' && <DebugSection />}
        </FoldRow>
      </Group>

      {me.admin && (
        <Group head={t('settings.admin')}>
          <button className="set-row set-open" onClick={openAdmin}>
            <span className="set-row-body">
              <span className="set-label">{t('settings.adminPanel')}</span>
            </span>
            <svg className="set-chev" viewBox="0 0 24 24" aria-hidden="true">
              <path d="M9 6l6 6-6 6" />
            </svg>
          </button>
        </Group>
      )}

      <button
        className="btn-haze small set-signout"
        onClick={async () => {
          try {
            await api.logout()
          } catch {
            // dropping to the login screen is the surface either way
          } finally {
            onSignedOut()
          }
        }}
      >
        {t('settings.signOut')}
      </button>
    </div>
  )
}

const MIN_PASSWORD = 8

function PasswordSection() {
  const [current, setCurrent] = useState('')
  const [next, setNext] = useState('')
  const [confirm, setConfirm] = useState('')
  const [save, setSave] = useState<Save>(null)

  const ready = current !== '' && next.length >= MIN_PASSWORD && next === confirm

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    if (!ready) return
    setSave({ row: 'password', kind: 'busy' })
    try {
      await api.changePassword(current, next)
      setCurrent('')
      setNext('')
      setConfirm('')
      setSave({ row: 'password', kind: 'saved', message: t('settings.password.changed') })
    } catch (err) {
      const message =
        err instanceof ApiError
          ? err.status === 401
            ? t('settings.password.wrong')
            : err.message
          : t('settings.password.failed')
      setSave({ row: 'password', kind: 'failed', message })
    }
  }

  return (
    <form className="set-fold-body" onSubmit={(e) => void submit(e)}>
      <input
        type="password"
        aria-label={t('settings.password.current')}
        placeholder={t('settings.password.current')}
        autoComplete="current-password"
        value={current}
        onChange={(e) => setCurrent(e.target.value)}
      />
      <input
        type="password"
        aria-label={t('settings.password.new')}
        placeholder={t('settings.password.new')}
        autoComplete="new-password"
        value={next}
        onChange={(e) => setNext(e.target.value)}
      />
      <input
        type="password"
        aria-label={t('settings.password.confirm')}
        placeholder={t('settings.password.confirm')}
        autoComplete="new-password"
        value={confirm}
        onChange={(e) => setConfirm(e.target.value)}
      />
      <div className="set-acts">
        <button type="submit" className="btn-haze small" disabled={!ready || save?.kind === 'busy'}>
          {t('settings.password.change')}
        </button>
        <Status save={save} row="password" />
      </div>
    </form>
  )
}

function ScheduleList({
  rows,
  busy,
  toggle,
}: {
  rows: ScheduleRow[]
  busy: boolean
  toggle: (row: ScheduleRow) => void
}) {
  if (rows.length === 0) return null
  return (
    <ul className="sched-list">
      {rows.map((row) => {
        const slide = slideWord(row)
        return (
        <li className="sched-row" key={row.index}>
          <span className="sched-time">
            {row.entry === 'block' ? `${row.time}–${row.end_time ?? ''}` : row.time}
          </span>
          <span className="set-row-body">
            <span className="set-label">{eventLabel(row.kind)}</span>
            {slide && <span className="set-sub">{slide}</span>}
          </span>
          {row.entry === 'routine' && (
            <Switch
              label={t('settings.schedule.pings', { name: eventLabel(row.kind) })}
              on={row.alert}
              disabled={busy}
              onToggle={() => toggle(row)}
            />
          )}
        </li>
        )
      })}
    </ul>
  )
}

function AboutSection({ active }: { active: boolean }) {
  const [saved, setSaved] = useState<string | 'error' | undefined>(undefined)
  const [text, setText] = useState<string | undefined>(undefined)
  const [save, setSave] = useState<Save>(null)
  const asked = useRef(false)

  const load = () => {
    asked.current = true
    setSaved(undefined)
    api
      .aboutGet()
      .then((d) => setSaved(d.content))
      .catch(() => {
        asked.current = false
        setSaved('error')
      })
  }

  useEffect(() => {
    if (active && !asked.current) load()
  }, [active])

  if (!active) return null

  if (saved === undefined || saved === 'error') {
    return (
      <div className="set-fold-body">
        {saved === 'error' && (
          <p className="set-sub">
            {t('settings.about.loadFailed')}{' '}
            <button className="set-link" onClick={load}>
              {t('common.retry')}
            </button>
          </p>
        )}
      </div>
    )
  }

  const value = text ?? saved
  const dirty = value !== saved
  const busy = save?.kind === 'busy'

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    if (!dirty) return
    setSave({ row: 'about', kind: 'busy' })
    try {
      const d = await api.aboutPut(value)
      setSaved(d.content)
      setText(undefined)
      setSave({ row: 'about', kind: 'saved' })
    } catch (err) {
      setSave({
        row: 'about',
        kind: 'failed',
        message: err instanceof ApiError && err.status === 400 ? err.message : t('settings.about.saveFailed'),
      })
    }
  }

  return (
    <form className="set-fold-body" onSubmit={submit}>
      <fieldset className="set-prompt-rows" disabled={busy}>
        <textarea
          className="set-prompt"
          aria-label={t('settings.about')}
          placeholder={t('settings.about.hint')}
          rows={8}
          value={value}
          onChange={(e) => setText(e.target.value)}
        />
      </fieldset>
      <div className="set-acts">
        <button className="btn-haze small" disabled={!dirty || busy}>
          {busy ? t('settings.about.saving') : t('common.save')}
        </button>
        {!dirty && <Status save={save} row="about" />}
      </div>
    </form>
  )
}

const PROMPT_KINDS: { id: PromptKind; key: Key }[] = [
  { id: 'talk', key: 'settings.debug.talk' },
  { id: 'trigger', key: 'settings.debug.trigger' },
  { id: 'nightly', key: 'settings.debug.nightly' },
  { id: 'call', key: 'settings.debug.call' },
]

function DebugSection() {
  const [kind, setKind] = useState<PromptKind>('talk')
  const [built, setBuilt] = useState<BuiltPrompt | 'error' | undefined>(undefined)
  const [asks, setAsks] = useState(0)

  useEffect(() => {
    let live = true
    setBuilt(undefined)
    api
      .debugPrompt(kind)
      .then((b) => live && setBuilt(b))
      .catch(() => live && setBuilt('error'))
    return () => {
      live = false
    }
  }, [kind, asks])

  return (
    <div className="set-fold-body">
      <div className="set-acts">
        <select aria-label={t('settings.debug.session')} value={kind} onChange={(e) => setKind(e.target.value as PromptKind)}>
          {PROMPT_KINDS.map((k) => (
            <option key={k.id} value={k.id}>
              {t(k.key)}
            </option>
          ))}
        </select>
        <button className="set-link" onClick={() => setAsks((n) => n + 1)}>
          {t('settings.debug.refresh')}
        </button>
      </div>
      {built === 'error' && <p className="set-sub">{t('settings.debug.failed')}</p>}
      {built && built !== 'error' && (
        <>
          <p className="set-sub">
            {t('settings.debug.size', { chars: built.prompt.length.toLocaleString(), tools: String(built.tools.length) })}
          </p>
          <pre className="mono set-debug">{built.prompt}</pre>
          <p className="set-sub mono">{built.tools.join(' · ')}</p>
        </>
      )}
    </div>
  )
}

const dayOf = (iso: string) => format.day(new Date(iso))

const passkeyLine = (k: Passkey) =>
  k.last_used_at
    ? t('settings.security.passkeyUsed', { added: dayOf(k.created_at), used: dayOf(k.last_used_at) })
    : t('settings.security.passkeyNew', { added: dayOf(k.created_at) })

// What the password prompt is standing in front of.
type Ask =
  | { kind: 'add-passkey' }
  | { kind: 'drop-passkey'; id: number }
  | { kind: 'start-totp' }
  | { kind: 'drop-totp' }

const ASK_LABEL: Record<Ask['kind'], { action: Key; password: Key }> = {
  'add-passkey': { action: 'settings.security.addPasskey', password: 'settings.security.passwordTo.addPasskey' },
  'drop-passkey': { action: 'settings.security.dropPasskey', password: 'settings.security.passwordTo.dropPasskey' },
  'start-totp': { action: 'settings.security.startTotp', password: 'settings.security.passwordTo.startTotp' },
  'drop-totp': { action: 'settings.security.dropTotp', password: 'settings.security.passwordTo.dropTotp' },
}

function QrCode({ uri, label }: { uri: string; label?: string }) {
  const canvas = useRef<HTMLCanvasElement>(null)
  const [drawn, setDrawn] = useState(true)

  useEffect(() => {
    const el = canvas.current
    if (!el) return
    // a plain white ground, so a scanner reads it in either theme
    QRCode.toCanvas(el, uri, { width: 168, margin: 1, color: { dark: '#000000', light: '#ffffff' } })
      .then(() => setDrawn(true))
      .catch(() => setDrawn(false))
  }, [uri])

  if (!drawn) return <span className="set-sub">{t('settings.security.manualKeyBelow')}</span>
  return <canvas className="set-qr" ref={canvas} aria-label={label ?? t('settings.security.qr')} />
}

function TotpEnrol({
  enrol,
  busy,
  onConfirm,
  onCancel,
}: {
  enrol: TotpEnrolment
  busy: boolean
  onConfirm: (code: string) => void
  onCancel: () => void
}) {
  const [code, setCode] = useState('')
  return (
    <form
      className="set-enrol"
      onSubmit={(e) => {
        e.preventDefault()
        onConfirm(code)
      }}
    >
      <QrCode uri={enrol.otpauth_uri} />
      <a className="set-link" href={enrol.otpauth_uri}>
        {t('settings.security.openManager')}
      </a>
      <span className="set-sub">{t('settings.security.manualKey')}</span>
      <code className="set-token-secret">{enrol.secret_base32}</code>
      <span className="set-sub">{t('settings.security.enterCode')}</span>
      <div className="set-token-form">
        <input
          aria-label={t('settings.security.code')}
          inputMode="numeric"
          autoComplete="one-time-code"
          pattern="\d{6}"
          maxLength={6}
          value={code}
          onChange={(e) => setCode(e.target.value)}
        />
        <button type="submit" className="btn-haze small" disabled={busy || code.length !== 6}>
          {t('settings.security.finish')}
        </button>
        <button type="button" className="set-link" onClick={onCancel}>
          {t('common.cancel')}
        </button>
      </div>
    </form>
  )
}

export function SecuritySection({
  notify,
  onState,
}: {
  notify: Notify
  onState?: (s: SecurityState) => void
}) {
  const [state, setState] = useState<SecurityState | 'error' | undefined>(undefined)
  const [ask, setAsk] = useState<Ask | null>(null)
  const [password, setPassword] = useState('')
  const [naming, setNaming] = useState<{ credential: RegistrationJSON; name: string } | null>(null)
  const [enrol, setEnrol] = useState<TotpEnrolment | null>(null)
  const [rename, setRename] = useState<{ id: number; name: string } | null>(null)
  // Remove is two taps, like revoking a token; the second opens the password prompt.
  const [arming, setArming] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const load = () => {
    security
      .state()
      .then((s) => {
        setState(s)
        onState?.(s)
      })
      .catch(() => setState('error'))
  }
  useEffect(load, [])

  const loaded = state !== undefined && state !== 'error' ? state : null
  const canPasskey = !!loaded?.webauthn_available && webauthnSupported()

  const fail = (err: unknown, fallback: string) =>
    notify(err instanceof ApiError && err.status < 500 && err.message ? err.message : fallback)

  const open = (kind: Ask) => {
    setAsk(kind)
    setPassword('')
    setArming(null)
  }

  const answer = async (e: FormEvent) => {
    e.preventDefault()
    if (!ask || busy) return
    setBusy(true)
    try {
      if (ask.kind === 'add-passkey') {
        const challenge = await security.passkeyChallenge(password)
        setAsk(null)
        setNaming({ credential: await createCredential(challenge), name: '' })
      } else if (ask.kind === 'drop-passkey') {
        await security.removePasskey(ask.id, password)
        setAsk(null)
        load()
      } else if (ask.kind === 'start-totp') {
        setEnrol(await security.totpStart(password))
        setAsk(null)
      } else {
        await security.totpRemove(password)
        setAsk(null)
        load()
      }
      setPassword('')
    } catch (err) {
      fail(err, t('settings.notThrough'))
    } finally {
      setBusy(false)
    }
  }

  const keepPasskey = async (e: FormEvent) => {
    e.preventDefault()
    if (!naming || busy) return
    setBusy(true)
    try {
      await security.addPasskey(naming.name.trim(), naming.credential)
      setNaming(null)
      load()
    } catch (err) {
      fail(err, t('settings.security.passkeyFailed'))
    } finally {
      setBusy(false)
    }
  }

  const commitRename = async () => {
    if (!rename) return
    const { id, name } = rename
    setRename(null)
    const was = loaded?.passkeys.find((k) => k.id === id)?.name
    if (!name.trim() || name.trim() === was) return
    try {
      await security.renamePasskey(id, name.trim())
      load()
    } catch (err) {
      fail(err, t('settings.security.nameFailed'))
    }
  }

  const confirmTotp = async (code: string) => {
    setBusy(true)
    try {
      await security.totpConfirm(code)
      setEnrol(null)
      load()
    } catch (err) {
      fail(err, t('settings.security.codeFailed'))
    } finally {
      setBusy(false)
    }
  }

  const removeButton = (key: string, onArmed: () => void, label: string) => (
    <button
      type="button"
      className="btn-haze small"
      onClick={() => (arming === key ? onArmed() : setArming(key))}
      onBlur={() => setArming((a) => (a === key ? null : a))}
    >
      {arming === key ? t('settings.security.reallyRemove') : label}
    </button>
  )

  return (
    <div className="set-fold-body">
      <span className="set-sub">{t('settings.security.sub')}</span>

      {state === 'error' && (
        <span className="set-sub">
          {t('settings.security.loadFailed')}{' '}
          <button className="set-link" onClick={load}>
            {t('common.retry')}
          </button>
        </span>
      )}

      {loaded?.passkeys.map((k) => (
        <div className="set-row set-token-row" key={k.id}>
          <span className="set-row-body">
            {rename?.id === k.id ? (
              <input
                aria-label={t('settings.security.passkeyName')}
                maxLength={64}
                autoFocus
                value={rename.name}
                onChange={(e) => setRename({ id: k.id, name: e.target.value })}
                onBlur={() => void commitRename()}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') e.currentTarget.blur()
                  if (e.key === 'Escape') setRename(null)
                }}
              />
            ) : (
              <button className="set-link set-name" onClick={() => setRename({ id: k.id, name: k.name })}>
                {k.name}
              </button>
            )}
            <span className="set-sub">{passkeyLine(k)}</span>
          </span>
          {removeButton(`passkey:${k.id}`, () => open({ kind: 'drop-passkey', id: k.id }), t('settings.security.remove'))}
        </div>
      ))}

      {loaded && (
        <div className="set-row set-token-row">
          <span className="set-row-body">
            <span className="set-label">{t('settings.security.app')}</span>
            <span className="set-sub">{loaded.totp.enabled ? t('settings.security.appOn') : t('settings.security.appHint')}</span>
          </span>
          {loaded.totp.enabled
            ? removeButton('totp', () => open({ kind: 'drop-totp' }), t('settings.security.remove'))
            : !enrol && (
                <button
                  type="button"
                  className="btn-haze small"
                  disabled={busy}
                  onClick={() => open({ kind: 'start-totp' })}
                >
                  {t('settings.security.startTotp')}
                </button>
              )}
        </div>
      )}

      {enrol && (
        <TotpEnrol
          enrol={enrol}
          busy={busy}
          onConfirm={(code) => void confirmTotp(code)}
          onCancel={() => setEnrol(null)}
        />
      )}

      {loaded && !naming && (
        <div className="set-acts">
          <button
            type="button"
            className="btn-haze small"
            disabled={!canPasskey || busy}
            onClick={() => open({ kind: 'add-passkey' })}
          >
            {t('settings.security.addPasskey')}
          </button>
          {!canPasskey && <span className="set-sub">{t('settings.security.passkeyHint')}</span>}
        </div>
      )}

      {ask && (
        <form className="set-token-form" onSubmit={(e) => void answer(e)}>
          <input
            type="password"
            aria-label={t(ASK_LABEL[ask.kind].password)}
            placeholder={t('settings.security.password')}
            autoComplete="current-password"
            autoFocus
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
          <button type="submit" className="btn-haze small" disabled={busy || !password}>
            {t(ASK_LABEL[ask.kind].action)}
          </button>
          <button type="button" className="set-link" onClick={() => setAsk(null)}>
            {t('common.cancel')}
          </button>
        </form>
      )}

      {naming && (
        <form className="set-token-form" onSubmit={(e) => void keepPasskey(e)}>
          <input
            aria-label={t('settings.security.namePasskey')}
            placeholder={t('settings.security.namePasskey')}
            maxLength={64}
            autoFocus
            value={naming.name}
            onChange={(e) => setNaming({ ...naming, name: e.target.value })}
          />
          <button type="submit" className="btn-haze small" disabled={busy || !naming.name.trim()}>
            {t('common.save')}
          </button>
        </form>
      )}
    </div>
  )
}

function TokensSection({ notify }: { notify: Notify }) {
  const [tokens, setTokens] = useState<Token[] | 'error' | undefined>(undefined)
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const [fresh, setFresh] = useState<TokenCreated | null>(null)
  // Revoke is two taps: the first arms the row, the second removes it.
  const [arming, setArming] = useState<number | null>(null)

  useEffect(() => {
    api
      .tokens()
      .then(setTokens)
      .catch(() => setTokens('error'))
  }, [])

  const create = async (e: FormEvent) => {
    e.preventDefault()
    const trimmed = name.trim()
    if (!trimmed || busy || !Array.isArray(tokens)) return
    setBusy(true)
    try {
      const made = await api.createToken(trimmed)
      const info: Token = {
        id: made.id,
        name: made.name,
        created_at: made.created_at,
        last_used_at: made.last_used_at,
      }
      setFresh(made)
      setName('')
      setTokens((all) => (Array.isArray(all) ? [...all, info] : all))
    } catch (err) {
      notify(
        err instanceof ApiError && (err.status === 409 || err.status === 422)
          ? err.message
          : t('settings.tokens.createFailed'),
      )
    } finally {
      setBusy(false)
    }
  }

  const revoke = async (token: Token) => {
    if (arming !== token.id) {
      setArming(token.id)
      return
    }
    setArming(null)
    try {
      await api.revokeToken(token.id)
      setTokens((all) => (Array.isArray(all) ? all.filter((x) => x.id !== token.id) : all))
      if (fresh?.id === token.id) setFresh(null)
    } catch {
      notify(t('settings.tokens.revokeFailed'))
    }
  }

  return (
    <div className="set-fold-body">
      <form className="set-token-form" onSubmit={(e) => void create(e)}>
        <input
          aria-label={t('settings.tokens.name')}
          placeholder={t('settings.tokens.namePlaceholder')}
          maxLength={64}
          spellCheck={false}
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <button
          type="submit"
          className="btn-haze small"
          disabled={busy || !name.trim() || !Array.isArray(tokens)}
        >
          {t('settings.tokens.create')}
        </button>
      </form>
      {fresh && (
        <div className="set-token-fresh">
          <code className="set-token-secret">{fresh.token}</code>
          <span className="set-sub">{t('settings.tokens.copyNow')}</span>
        </div>
      )}
      {tokens === 'error' && <span className="set-sub">{t('settings.tokens.loadFailed')}</span>}
      {Array.isArray(tokens) && tokens.length === 0 && (
        <span className="set-sub">{t('settings.tokens.none')}</span>
      )}
      {Array.isArray(tokens) &&
        tokens.map((token) => (
          <div className="set-row set-token-row" key={token.id}>
            <span className="set-row-body">
              <span className="set-label">{token.name}</span>
              <span className="set-sub">
                {token.last_used_at
                  ? t('settings.tokens.used', {
                      created: dayOf(token.created_at),
                      used: dayOf(token.last_used_at),
                    })
                  : t('settings.tokens.new', { created: dayOf(token.created_at) })}
              </span>
            </span>
            <button
              type="button"
              className="btn-haze small"
              onClick={() => void revoke(token)}
              onBlur={() => setArming((a) => (a === token.id ? null : a))}
            >
              {arming === token.id ? t('settings.tokens.reallyRevoke') : t('settings.tokens.revoke')}
            </button>
          </div>
        ))}
    </div>
  )
}

const EXPIRIES = [7, 30, 120] as const

const DEFAULT_SCOPE: ShareScope = {
  today: true, tasks: true, categories: [], goals: true, progress: true,
  details: false, horizon_days: 3, notes: false, messages_per_day: 40,
}

function inDays(days: number): string {
  return new Date(Date.now() + days * 24 * 60 * 60 * 1000).toISOString()
}

function scopeWords(s: ShareScope): string {
  const parts: string[] = []
  if (s.tasks) parts.push(...(s.categories.length > 0 ? s.categories : [t('settings.share.scope.tasks')]))
  if (s.today) parts.push(t('settings.share.scope.plan'))
  if (s.goals) parts.push(t('settings.share.scope.goals'))
  if (s.progress) parts.push(t('settings.share.scope.progress'))
  const words = parts.length > 0 ? format.unitList(parts) : t('settings.share.scope.nothing')
  return words.charAt(0).toUpperCase() + words.slice(1)
}

function localToday(): string {
  const d = new Date()
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
}

function clampedOr(raw: string, current: number, min: number, max: number): number {
  const n = Number(raw)
  if (raw.trim() === '' || !Number.isFinite(n)) return current
  return Math.min(max, Math.max(min, Math.round(n)))
}

const MESSAGE_FROM: Record<ShareMessage['role'], Key> = {
  note: 'settings.share.fromNote',
  user: 'settings.share.fromThem',
  assistant: 'settings.share.fromAssistant',
}

type ShareDraft = { name: string; brief: string; scope: ShareScope; days: number; date: string }

function ShareForm({
  initial,
  categories,
  submit,
  busy,
  label,
  expiryNote,
}: {
  initial: ShareDraft
  categories: string[]
  submit: (d: ShareDraft) => void
  busy: boolean
  label: string
  expiryNote?: string
}) {
  const [d, setD] = useState<ShareDraft>(initial)
  const pills = [...new Set([...categories, ...d.scope.categories])].sort()
  const filtered = d.scope.today || d.scope.tasks || d.scope.goals || d.scope.progress
  const scope = (patch: Partial<ShareScope>) => setD((x) => ({ ...x, scope: { ...x.scope, ...patch } }))
  const toggleCategory = (c: string) =>
    scope({
      categories: d.scope.categories.includes(c)
        ? d.scope.categories.filter((x) => x !== c)
        : [...d.scope.categories, c],
    })
  return (
    <form
      className="set-share-form"
      onSubmit={(e) => {
        e.preventDefault()
        submit(d)
      }}
    >
      <input aria-label={t('settings.share.linkName')} placeholder={t('settings.share.whoFor')} maxLength={64} value={d.name} onChange={(e) => setD({ ...d, name: e.target.value })} />
      {expiryNote && <span className="set-sub">{expiryNote}</span>}
      <div className="set-row set-share-expiry">
        <span className="set-row-body"><span className="set-label">{t('settings.share.endsAfter')}</span></span>
        <div className="seg" role="group" aria-label={t('settings.share.expiry')}>
          {EXPIRIES.map((days) => (
            <button key={days} type="button" aria-pressed={d.days === days && d.date === ''} onClick={() => setD({ ...d, days, date: '' })}>{t('settings.share.days', { count: days })}</button>
          ))}
        </div>
      </div>
      <div className="set-row">
        <span className="set-row-body"><span className="set-label">{t('settings.share.orEndsOn')}</span></span>
        <input type="date" aria-label={t('settings.share.endsOn')} min={localToday()} value={d.date} onChange={(e) => setD({ ...d, date: e.target.value })} />
      </div>
      <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.plan')}</span></span><Switch label={t('settings.share.sharePlan')} on={d.scope.today} onToggle={() => scope({ today: !d.scope.today })} /></div>
      {d.scope.today && (
        <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.daysAhead')}</span></span><input type="number" aria-label={t('settings.share.daysAhead')} min={1} max={14} value={d.scope.horizon_days} onChange={(e) => scope({ horizon_days: clampedOr(e.target.value, d.scope.horizon_days, 1, 14) })} /></div>
      )}
      <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.tasks')}</span></span><Switch label={t('settings.share.shareTasks')} on={d.scope.tasks} onToggle={() => scope({ tasks: !d.scope.tasks })} /></div>
      {d.scope.tasks && (
        <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.details')}</span></span><Switch label={t('settings.share.shareDetails')} on={d.scope.details} onToggle={() => scope({ details: !d.scope.details })} /></div>
      )}
      <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.goals')}</span></span><Switch label={t('settings.share.shareGoals')} on={d.scope.goals} onToggle={() => scope({ goals: !d.scope.goals })} /></div>
      <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.progress')}</span></span><Switch label={t('settings.share.shareProgress')} on={d.scope.progress} onToggle={() => scope({ progress: !d.scope.progress })} /></div>
      {filtered && pills.length > 0 && (
        <div className="set-share-cats">
          <span className="set-sub">{t('settings.share.categoriesSub')}</span>
          <div className="seg wrap" role="group" aria-label={t('settings.share.categories')}>
            {pills.map((c) => (
              <button key={c} type="button" aria-pressed={d.scope.categories.includes(c)} onClick={() => toggleCategory(c)}>{c}</button>
            ))}
          </div>
        </div>
      )}
      <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.notes')}</span></span><Switch label={t('settings.share.allowNotes')} on={d.scope.notes} onToggle={() => scope({ notes: !d.scope.notes })} /></div>
      <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.messages')}</span></span><input type="number" aria-label={t('settings.share.messages')} min={1} max={100} value={d.scope.messages_per_day} onChange={(e) => scope({ messages_per_day: clampedOr(e.target.value, d.scope.messages_per_day, 1, 100) })} /></div>
      <textarea aria-label={t('settings.share.brief')} placeholder={t('settings.share.briefHint')} rows={3} maxLength={4096} value={d.brief} onChange={(e) => setD({ ...d, brief: e.target.value })} />
      <span className="set-sub">{t('settings.share.briefSub')}</span>
      <button type="submit" className="btn-haze small" disabled={busy || !d.name.trim()}>{label}</button>
    </form>
  )
}

function SharesSection({ notify }: { notify: Notify }) {
  const [shares, setShares] = useState<Share[] | 'error' | undefined>(undefined)
  const [categories, setCategories] = useState<string[]>([])
  const [creating, setCreating] = useState(false)
  const [editing, setEditing] = useState<number | null>(null)
  const [openThreads, setOpenThreads] = useState<number | null>(null)
  const [threads, setThreads] = useState<ShareThread[] | undefined>(undefined)
  const [visits, setVisits] = useState<ShareVisit[] | undefined>(undefined)
  const [busy, setBusy] = useState(false)
  const [fresh, setFresh] = useState<Share | null>(null)
  const threadsFor = useRef<number | null>(null)

  useEffect(() => {
    api.shares().then(setShares).catch(() => setShares('error'))
    api
      .tasks()
      .then((ts) => setCategories([...new Set(ts.map((x) => x.category).filter((c) => c !== ''))].sort()))
      .catch(() => setCategories([]))
  }, [])

  const expiresOf = (d: ShareDraft) => (d.date ? new Date(`${d.date}T23:59:00`).toISOString() : inDays(d.days))

  const create = async (d: ShareDraft) => {
    setBusy(true)
    try {
      const made = await api.createShare({ name: d.name.trim(), brief: d.brief, scope: d.scope, expires_at: expiresOf(d) })
      setShares((all) => (Array.isArray(all) ? [...all, made] : all))
      setFresh(made)
      setCreating(false)
    } catch (err) {
      notify(err instanceof ApiError && (err.status === 409 || err.status === 422) ? err.message : t('settings.share.createFailed'))
    } finally {
      setBusy(false)
    }
  }

  const save = async (s: Share, d: ShareDraft) => {
    setBusy(true)
    try {
      const body: SharePatch = { name: d.name.trim(), brief: d.brief, scope: d.scope }
      if (d.date || d.days !== 0) body.expires_at = expiresOf(d)
      const up = await api.updateShare(s.id, body)
      setShares((all) => (Array.isArray(all) ? all.map((x) => (x.id === s.id ? up : x)) : all))
      setEditing(null)
      notify(t('settings.share.saved'))
    } catch (err) {
      notify(err instanceof ApiError && err.status === 422 ? err.message : t('settings.share.saveFailed'))
    } finally {
      setBusy(false)
    }
  }

  const doRevoke = async (s: Share) => {
    try {
      await api.revokeShare(s.id)
      setShares((all) => (Array.isArray(all) ? all.filter((x) => x.id !== s.id) : all))
      if (fresh?.id === s.id) setFresh(null)
      notify(t('settings.share.revoked'))
    } catch {
      notify(t('settings.share.revokeFailed'))
    }
  }

  const copy = (s: Share) => {
    const fallback = () => notify(t('settings.share.copyFallback'))
    if (navigator.clipboard) navigator.clipboard.writeText(s.url).then(() => notify(t('settings.share.copied')), fallback)
    else fallback()
    setFresh(s)
  }

  const showThreads = (s: Share) => {
    if (openThreads === s.id) {
      threadsFor.current = null
      setOpenThreads(null)
      return
    }
    threadsFor.current = s.id
    setOpenThreads(s.id)
    setThreads(undefined)
    setVisits(undefined)
    const land = (ts: ShareThread[]) => {
      if (threadsFor.current === s.id) setThreads(ts)
    }
    const landVisits = (vs: ShareVisit[]) => {
      if (threadsFor.current === s.id) setVisits(vs)
    }
    api.shareThreads(s.id).then(land).catch(() => land([]))
    api.shareVisits(s.id).then(landVisits).catch(() => landVisits([]))
  }

  const blank: ShareDraft = { name: '', brief: '', scope: DEFAULT_SCOPE, days: 30, date: '' }
  const draftOf = (s: Share): ShareDraft => ({ name: s.name, brief: s.brief, scope: s.scope, days: 0, date: '' })

  return (
    <div className="set-fold-body">
      <span className="set-sub">{t('settings.share.intro')}</span>
      {!creating && <button type="button" className="btn-haze small" onClick={() => setCreating(true)}>{t('settings.share.new')}</button>}
      {creating && <ShareForm initial={blank} categories={categories} submit={(d) => void create(d)} busy={busy} label={t('settings.share.create')} />}
      {fresh && (
        <div className="set-token-fresh">
          <code className="set-token-secret">{fresh.url}</code>
          <span className="set-sub">{t('settings.share.anyone', { name: fresh.name, day: dayOf(fresh.expires_at) })}</span>
        </div>
      )}
      {shares === 'error' && <span className="set-sub">{t('settings.share.loadFailed')}</span>}
      {Array.isArray(shares) && shares.length === 0 && !creating && <span className="set-sub">{t('settings.share.none')}</span>}
      {Array.isArray(shares) &&
        shares.map((s) => (
          <div key={s.id} className="set-share">
            <div className="set-row set-token-row">
              <span className="set-row-body">
                <span className="set-label">
                  {s.name}
                  {s.distant_visits > 0 && <i className="set-share-far" role="img" aria-label={t('settings.share.far')} title={t('settings.share.far')} />}
                </span>
                <span className="set-sub">
                  {t('settings.share.summary', {
                    scope: scopeWords(s.scope),
                    day: dayOf(s.expires_at),
                    visitors: s.visitors,
                    messages: s.messages_today,
                  })}
                </span>
              </span>
              <Overflow
                className="set-share-more"
                label={t('settings.share.moreFor', { name: s.name })}
                items={[
                  { label: t('settings.share.copy'), run: () => copy(s) },
                  { label: t('settings.share.preview'), run: () => window.open(s.url, '_blank', 'noopener') },
                  {
                    label: openThreads === s.id ? t('settings.share.hideActivity') : t('settings.share.activity'),
                    run: () => showThreads(s),
                  },
                  {
                    label: editing === s.id ? t('settings.share.stopEditing') : t('menu.edit'),
                    run: () => setEditing(editing === s.id ? null : s.id),
                  },
                  {
                    label: t('settings.share.revoke'),
                    kind: 'danger',
                    run: () =>
                      notify(t('settings.share.revokeAsk'), {
                        label: t('settings.share.revoke'),
                        run: () => void doRevoke(s),
                      }),
                  },
                ]}
              />
            </div>
            {editing === s.id && (
              <ShareForm
                initial={draftOf(s)}
                categories={categories}
                submit={(d) => void save(s, d)}
                busy={busy}
                label={t('common.save')}
                expiryNote={t('settings.share.currently', { day: dayOf(s.expires_at) })}
              />
            )}
            {openThreads === s.id && (
              <div className="set-share-threads">
                {visits && visits.length > 0 && (
                  <ul className="set-share-visits">
                    {visits.map((v, i) => (
                      <li key={i} className={v.distant ? 'far' : undefined}>
                        {[
                          [v.city, v.country].filter(Boolean).join(', ') || t('settings.share.somewhere'),
                          v.km === null ? null : t('settings.share.km', { n: format.number(Math.round(v.km)) }),
                          dayOf(v.at),
                        ]
                          .filter(Boolean)
                          .join(' · ')}
                      </li>
                    ))}
                  </ul>
                )}
                {threads === undefined && <span className="set-sub">{t('settings.share.loading')}</span>}
                {threads && threads.length === 0 && <span className="set-sub">{t('settings.share.noQuestions')}</span>}
                {threads?.map((th) => (
                  <div key={th.id} className="set-share-thread">
                    <span className="set-sub">
                      {t('settings.share.visitor', { from: dayOf(th.created_at), last: dayOf(th.updated_at) })}
                    </span>
                    {th.messages.map((m, i) => (
                      <p key={i} className={`set-share-msg ${m.role}`}>
                        {t(MESSAGE_FROM[m.role], { text: m.content })}
                      </p>
                    ))}
                  </div>
                ))}
              </div>
            )}
          </div>
        ))}
    </div>
  )
}

function PushRow({ notify }: { notify: Notify }) {
  const [state, setState] = useState<'unsupported' | 'off' | 'on' | 'busy'>('busy')

  useEffect(() => {
    pushState()
      .then(setState)
      .catch(() => setState('unsupported'))
  }, [])

  const toggle = async () => {
    const was = state
    setState('busy')
    try {
      if (was === 'on') {
        await disablePush()
        setState('off')
      } else {
        await enablePush()
        setState('on')
      }
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) {
        notify(t('settings.push.unconfigured'))
      } else {
        notify(t('settings.push.failed'))
      }
      setState(was)
    }
  }

  return (
    <div className="set-row">
      <span className="set-row-body">
        <span className="set-label">{t('settings.push')}</span>
        {state === 'unsupported' && <span className="set-sub">{t('settings.push.unsupported')}</span>}
      </span>
      <Switch
        label={t('settings.push')}
        on={state === 'on'}
        disabled={state === 'busy' || state === 'unsupported'}
        onToggle={() => void toggle()}
      />
    </div>
  )
}
