import QRCode from 'qrcode'
import { useEffect, useMemo, useRef, useState, type FormEvent, type ReactNode } from 'react'
import { api, ApiError, security } from '../api'
import type { ToastAction, ViewProps } from '../app'
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
  deviceZone,
  paintSky,
  savePlace,
  saveTheme,
  storedPlace,
  storedTheme,
  type ThemeChoice,
} from '../theme'
import type {
  Me,
  Passkey,
  PromptDoc,
  PromptName,
  ScheduleRow,
  SecurityState,
  Settings as UserSettings,
  Share,
  SharePatch,
  ShareScope,
  ShareThread,
  TelegramLink,
  Token,
  TokenCreated,
  TotpEnrolment,
} from '../types'
import { createCredential, webauthnSupported, type RegistrationJSON } from '../webauthn'

type Notify = (msg: string, action?: ToastAction) => void

const EDITABLE = [
  'display_name',
  'timezone',
  'nightly_time',
  'close_day_time',
  'template',
  'triggers_per_day',
  'pomodoro_work_min',
  'pomodoro_break_min',
] as const

const MAX_TRIGGERS_PER_DAY = 20
const WORK_MIN = { min: 5, max: 120 }
const BREAK_MIN = { min: 1, max: 60 }

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
  telegramEnabled: boolean
  telegramLinked: boolean
  telegramBot: string
}
type Save = { row: string; kind: 'busy' | 'saved' | 'failed'; message?: string } | null

const THEMES: { id: ThemeChoice; label: string }[] = [
  { id: 'system', label: 'System' },
  { id: 'light', label: 'Light' },
  { id: 'dark', label: 'Dark' },
  { id: 'sky', label: 'Sky' },
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
        <span>{times.rise ? `${times.rise} rise` : 'no sunrise'}</span>
        <span>{times.set ? `${times.set} set` : 'no sunset'}</span>
        <span>24:00</span>
      </div>
    </div>
  )
}

const COUNTERS: { id: CounterMode; label: string }[] = [
  { id: 'remaining', label: 'Remaining' },
  { id: 'elapsed', label: 'Elapsed' },
]

function draftOf(s: UserSettings): Draft {
  return {
    display_name: s.display_name,
    timezone: s.timezone,
    nightly_time: s.nightly_time,
    close_day_time: s.close_day_time,
    template: s.template,
    triggers_per_day: s.triggers_per_day,
    pomodoro_work_min: s.pomodoro_work_min,
    pomodoro_break_min: s.pomodoro_break_min,
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
  return min % 60 === 0 ? `± ${min / 60} h` : `± ${min} min`
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
        {save.message ?? '✓ Saved'}
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
  onSignedOut,
  openAdmin,
}: ViewProps & { me: Me; onSignedOut: () => void; openAdmin: () => void }) {
  const [state, setState] = useState<Loaded | 'error' | undefined>(undefined)
  const [save, setSave] = useState<Save>(null)
  const [open, setOpen] = useState<string | null>(null)
  const [theme, setTheme] = useState<ThemeChoice>(storedTheme)
  const [place, setPlace] = useState(storedPlace)
  const [invite, setInvite] = useState<TelegramLink | null>(null)

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
          telegramEnabled: s.telegram_enabled,
          telegramLinked: s.telegram_linked,
          telegramBot: s.telegram_bot,
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
      : "That didn't save. Try again."

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

  const commitFeature = async (
    row: string,
    patch: { nightly_enabled?: boolean; checkins_enabled?: boolean; pomodoro_enabled?: boolean },
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
            }
          : s,
      )
      setSave({ row, kind: 'saved' })
    } catch (err) {
      setSave({ row, kind: 'failed', message: failure(err) })
    }
  }

  const startLink = async () => {
    setSave({ row: 'telegram', kind: 'busy' })
    try {
      setInvite(await api.telegramLink())
      setSave(null)
    } catch (err) {
      setSave({ row: 'telegram', kind: 'failed', message: failure(err) })
    }
  }

  const unlinkTelegram = async () => {
    setSave({ row: 'telegram', kind: 'busy' })
    try {
      await api.telegramUnlink()
      setInvite(null)
      setState((s) => (s && s !== 'error' ? { ...s, telegramLinked: false } : s))
      setSave({ row: 'telegram', kind: 'saved' })
    } catch (err) {
      setSave({ row: 'telegram', kind: 'failed', message: failure(err) })
    }
  }

  // The link is finished over in Telegram, so the card watches for it rather
  // than asking for a reload.
  useEffect(() => {
    if (!invite) return
    const id = window.setInterval(() => {
      void api
        .settings()
        .then((s) => {
          if (!s.telegram_linked) return
          setInvite(null)
          setState((prev) =>
            prev && prev !== 'error' ? { ...prev, telegramLinked: true } : prev,
          )
        })
        .catch(() => undefined)
    }, 3000)
    return () => window.clearInterval(id)
  }, [invite])

  const sendTest = async () => {
    setSave({ row: 'test', kind: 'busy' })
    try {
      const { via } = await api.notifyTest()
      setSave({ row: 'test', kind: 'saved', message: `✓ Sent via ${via}` })
    } catch {
      setSave({ row: 'test', kind: 'failed', message: "That didn't reach you. Try again." })
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

  const locate = () => {
    if (!navigator.geolocation) return notify("Couldn't get your location.")
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
      () => notify("Couldn't get your location."),
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
  const themeLabel = THEMES.find((t) => t.id === theme)?.label ?? 'System'
  const counterLabel = COUNTERS.find((c) => c.id === loaded?.counter)?.label ?? 'Remaining'

  return (
    <div className="settings">
      {state === 'error' && (
        <p className="set-sub">
          Settings didn't load.{' '}
          <button className="set-link" onClick={load}>
            Retry
          </button>
        </p>
      )}

      {loaded && (
        <Group head="Sessions">
          <FoldRow
            label="Pomodoro"
            value={
              loaded.pomodoro
                ? `${loaded.draft.pomodoro_work_min} / ${loaded.draft.pomodoro_break_min} min`
                : 'Off'
            }
            open={open === 'pomodoro'}
            onToggle={fold('pomodoro')}
          >
            {open === 'pomodoro' && (
              <div className="set-fold-body">
                <span className="set-sub">
                  A session runs in rounds: work, then a break, and Note says when each
                  one is up.
                </span>
                <div className="set-row">
                  <span className="set-row-body">
                    <span className="set-label">Rounds</span>
                  </span>
                  <Switch
                    label="Pomodoro"
                    on={loaded.pomodoro}
                    disabled={busy}
                    onToggle={() =>
                      void commitFeature('pomodoro', { pomodoro_enabled: !loaded.pomodoro })
                    }
                  />
                </div>
                <label className="set-sub" htmlFor="pomodoro-work">
                  Work, in minutes
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
                  Break, in minutes
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
          <FoldRow
            label="Counter"
            value={counterLabel}
            open={open === 'counter'}
            onToggle={fold('counter')}
          >
            {open === 'counter' && (
              <div className="set-fold-body">
                <div className="seg" role="group" aria-label="Counter">
                  {COUNTERS.map((c) => (
                    <button
                      key={c.id}
                      type="button"
                      aria-pressed={loaded.counter === c.id}
                      onClick={() => void commitHome('counter', { counter: c.id })}
                    >
                      {c.label}
                    </button>
                  ))}
                </div>
                <Status save={save} row="counter" />
              </div>
            )}
          </FoldRow>
          <div className="set-row">
            <span className="set-row-body">
              <span className="set-label">Show the wait as an arc</span>
            </span>
            <Status save={save} row="arc" />
            <Switch
              label="Show the wait as an arc"
              on={loaded.arc}
              disabled={busy}
              onToggle={() => void commitHome('arc', { show_arc_between_sessions: !loaded.arc })}
            />
          </div>
        </Group>
      )}

      {loaded && (
        <Group head="Day">
          <FoldRow
            label="Routines and blocks"
            value={loaded.draft.template}
            open={open === 'schedule'}
            onToggle={fold('schedule')}
          >
            {open === 'schedule' && (
              <div className="set-fold-body">
                {templateChoices(loaded).length > 1 && (
                  <select
                    aria-label="Shape of the day"
                    value={loaded.draft.template}
                    onChange={(e) => {
                      edit('template', e.target.value)
                      void commit('schedule', { template: e.target.value })
                    }}
                  >
                    {templateChoices(loaded).map((t) => (
                      <option key={t} value={t}>
                        {t}
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
            label="Nightly plan"
            value={loaded.nightly ? `On, ${loaded.draft.nightly_time}` : 'Off'}
            open={open === 'nightly'}
            onToggle={fold('nightly')}
          >
            {open === 'nightly' && (
              <div className="set-fold-body">
                <div className="set-row">
                  <span className="set-row-body">
                    <span className="set-label">Plans the day and writes the letter</span>
                  </span>
                  <Status save={save} row="nightly_enabled" />
                  <Switch
                    label="Nightly plan"
                    on={loaded.nightly}
                    disabled={busy}
                    onToggle={() =>
                      void commitFeature('nightly_enabled', { nightly_enabled: !loaded.nightly })
                    }
                  />
                </div>
                <label className="set-sub" htmlFor="nightly-time">
                  Lands at
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
            label="Close the day"
            value={loaded.draft.close_day_time || 'Off'}
            open={open === 'close_day'}
            onToggle={fold('close_day')}
          >
            {open === 'close_day' && (
              <div className="set-fold-body">
                <span className="set-sub">
                  At this hour Note asks what is still open and offers to carry it to
                  tomorrow. Clear the time to leave the day to end on its own.
                </span>
                <input
                  type="time"
                  aria-label="Close the day"
                  value={loaded.draft.close_day_time}
                  onChange={(e) => edit('close_day_time', e.target.value)}
                  {...commitOn('close_day')}
                />
                <Status save={save} row="close_day" />
              </div>
            )}
          </FoldRow>
          <FoldRow
            label="Time zone"
            value={zoneCity(loaded.draft.timezone)}
            open={open === 'timezone'}
            onToggle={fold('timezone')}
          >
            {open === 'timezone' && (
              <div className="set-fold-body">
                <input
                  list="tz-list"
                  aria-label="Time zone"
                  spellCheck={false}
                  autoCapitalize="none"
                  value={loaded.draft.timezone}
                  onChange={(e) => edit('timezone', e.target.value)}
                  {...commitOn('timezone')}
                />
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

      <Group head="Notifications">
        <PushRow notify={notify} />
        {loaded?.telegramEnabled && (
          <FoldRow
            label="Telegram"
            value={loaded.telegramLinked ? `@${loaded.telegramBot}` : 'Not linked'}
            open={open === 'telegram'}
            onToggle={fold('telegram')}
          >
            {open === 'telegram' && (
              <div className="set-fold-body">
                {loaded.telegramLinked ? (
                  <>
                    <span className="set-sub">
                      Note answers you where you last wrote; the app keeps the whole record.
                    </span>
                    <button
                      type="button"
                      className="btn-haze small"
                      disabled={busy}
                      onClick={() => void unlinkTelegram()}
                    >
                      Unlink
                    </button>
                  </>
                ) : invite ? (
                  <div className="set-enrol">
                    <QrCode uri={invite.url} label="Telegram link QR code" />
                    <a
                      className="set-link"
                      href={invite.url}
                      target="_blank"
                      rel="noreferrer"
                    >
                      Open @{invite.bot}
                    </a>
                    <span className="set-sub">Or send the bot this code</span>
                    <code className="set-token-secret">/start {invite.code}</code>
                    <span className="set-sub">It lasts ten minutes</span>
                    <button
                      type="button"
                      className="btn-haze small"
                      disabled={busy}
                      onClick={() => void startLink()}
                    >
                      New code
                    </button>
                  </div>
                ) : (
                  <button
                    type="button"
                    className="btn-haze small"
                    disabled={busy}
                    onClick={() => void startLink()}
                  >
                    Link a chat
                  </button>
                )}
                <Status save={save} row="telegram" />
              </div>
            )}
          </FoldRow>
        )}
        {loaded && (
          <div className="set-row">
            <span className="set-row-body">
              <span className="set-label">Check-ins</span>
              <span className="set-sub">Routines on the day reach you when they fire</span>
            </span>
            <Status save={save} row="checkins_enabled" />
            <Switch
              label="Check-ins"
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
            label="Check-ins from Note"
            value={`Up to ${loaded.draft.triggers_per_day} a day`}
            open={open === 'triggers'}
            onToggle={fold('triggers')}
          >
            {open === 'triggers' && (
              <div className="set-fold-body">
                <span className="set-sub">
                  Moments Note sets aside to look at the day and reach out on its own. It asks
                  before it goes past this.
                </span>
                <input
                  aria-label="Check-ins from Note a day"
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
                    Send a test notification
                  </button>
                  <Status save={save} row="test" />
                </div>
              </div>
            )}
          </FoldRow>
        )}
      </Group>

      <Group head="You">
        {loaded && (
          <FoldRow
            label="Your name"
            value={loaded.draft.display_name}
            open={open === 'name'}
            onToggle={fold('name')}
          >
            {open === 'name' && (
              <div className="set-fold-body">
                <input
                  aria-label="Your name"
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
        <FoldRow label="Password" open={open === 'password'} onToggle={fold('password')}>
          {open === 'password' && <PasswordSection />}
        </FoldRow>
        <FoldRow
          label="Theme"
          value={themeLabel}
          open={open === 'theme'}
          onToggle={fold('theme')}
        >
          {open === 'theme' && (
            <div className="set-fold-body">
              <div className="seg" role="group" aria-label="Theme">
                {THEMES.map((t) => (
                  <button
                    key={t.id}
                    type="button"
                    aria-pressed={theme === t.id}
                    onClick={() => chooseTheme(t.id)}
                  >
                    {t.label}
                  </button>
                ))}
              </div>
              {theme === 'sky' && (
                <>
                  <SkyStrip key={place ? `${place.lat},${place.lon}` : 'zone'} />
                  <p className="set-note">
                    <button type="button" className="link" onClick={place ? forget : locate}>
                      {place ? 'Forget my location' : 'Use my location'}
                    </button>
                  </p>
                </>
              )}
            </div>
          )}
        </FoldRow>
      </Group>

      <Group head="Advanced">
        {/* The prompt editor stays mounted and renders nothing while closed, so an
            unsaved draft survives a detour through another row. */}
        <FoldRow label="How Note talks" open={open === 'persona'} onToggle={fold('persona')}>
          <PersonaSection active={open === 'persona'} notify={notify} />
        </FoldRow>
        <FoldRow label="Passkeys and codes" open={open === 'security'} onToggle={fold('security')}>
          {open === 'security' && <SecuritySection notify={notify} />}
        </FoldRow>
        <FoldRow label="API tokens" open={open === 'tokens'} onToggle={fold('tokens')}>
          {open === 'tokens' && <TokensSection notify={notify} />}
        </FoldRow>
        <FoldRow label="Share links" open={open === 'shares'} onToggle={fold('shares')}>
          {open === 'shares' && <SharesSection notify={notify} />}
        </FoldRow>
      </Group>

      {me.admin && (
        <Group head="Admin">
          <button className="set-row set-open" onClick={openAdmin}>
            <span className="set-row-body">
              <span className="set-label">Admin panel</span>
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
        Sign out
      </button>
    </div>
  )
}

const MIN_PASSWORD = 8
const WRONG_PASSWORD = "That isn't your current password."

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
      setSave({ row: 'password', kind: 'saved', message: 'Changed' })
    } catch (err) {
      const message =
        err instanceof ApiError
          ? err.status === 401
            ? WRONG_PASSWORD
            : err.message
          : "Couldn't change the password. Try again."
      setSave({ row: 'password', kind: 'failed', message })
    }
  }

  return (
    <form className="set-fold-body" onSubmit={(e) => void submit(e)}>
      <input
        type="password"
        aria-label="Current password"
        placeholder="Current password"
        autoComplete="current-password"
        value={current}
        onChange={(e) => setCurrent(e.target.value)}
      />
      <input
        type="password"
        aria-label="New password"
        placeholder="New password"
        autoComplete="new-password"
        value={next}
        onChange={(e) => setNext(e.target.value)}
      />
      <input
        type="password"
        aria-label="Confirm the new password"
        placeholder="Confirm the new password"
        autoComplete="new-password"
        value={confirm}
        onChange={(e) => setConfirm(e.target.value)}
      />
      <div className="set-acts">
        <button type="submit" className="btn-haze small" disabled={!ready || save?.kind === 'busy'}>
          Change password
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
              label={`${eventLabel(row.kind)} pings you`}
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

const PROMPTS: { id: PromptName; label: string }[] = [
  { id: 'persona', label: 'Persona' },
  { id: 'planning', label: 'Planning' },
  { id: 'share', label: 'Share links' },
]

const UNDO_MS = 5000

function PersonaSection({ active, notify }: { active: boolean; notify: Notify }) {
  const [name, setName] = useState<PromptName>('persona')
  // Fetched body and unsaved text are both kept per prompt, so switching the picker
  // holds on to a draft instead of discarding it.
  const [docs, setDocs] = useState<Partial<Record<PromptName, PromptDoc | 'error'>>>({})
  const [edits, setEdits] = useState<Partial<Record<PromptName, string>>>({})
  const [save, setSave] = useState<Save>(null)
  const [resets, setResets] = useState(0)
  const asked = useRef(new Set<PromptName>())
  const editor = useRef<HTMLTextAreaElement>(null)

  const load = (which: PromptName) => {
    asked.current.add(which)
    setDocs((all) => ({ ...all, [which]: undefined }))
    setSave(null)
    api
      .promptGet(which)
      .then((d) => setDocs((all) => ({ ...all, [which]: d })))
      .catch(() => {
        asked.current.delete(which)
        setDocs((all) => ({ ...all, [which]: 'error' }))
      })
  }

  useEffect(() => {
    if (active && !asked.current.has(name)) load(name)
  }, [active, name])

  // The reset button disables itself once the default lands, so focus goes to the
  // field the reset rewrote instead of falling to the body.
  useEffect(() => {
    if (resets) editor.current?.focus()
  }, [resets])

  if (!active) return null

  const doc = docs[name]
  const picker = (
    <select
      aria-label="Prompt"
      value={name}
      onChange={(e) => {
        setSave(null)
        setName(e.target.value as PromptName)
      }}
    >
      {PROMPTS.map((p) => (
        <option key={p.id} value={p.id}>
          {p.label}
        </option>
      ))}
    </select>
  )

  if (doc === undefined || doc === 'error') {
    return (
      <div className="set-fold-body">
        {picker}
        {doc === 'error' && (
          <p className="set-sub">
            The prompt didn't load.{' '}
            <button className="set-link" onClick={() => load(name)}>
              Retry
            </button>
          </p>
        )}
      </div>
    )
  }

  const text = edits[name] ?? doc.content
  const blank = text.trim().length === 0
  const dirty = text !== doc.content
  const busy = save?.kind === 'busy'
  const setText = (value: string) => setEdits((all) => ({ ...all, [name]: value }))

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    if (!dirty || blank) return
    setSave({ row: 'prompt', kind: 'busy' })
    try {
      const d = await api.promptPut(name, text)
      setDocs((all) => ({ ...all, [name]: d }))
      setSave({ row: 'prompt', kind: 'saved' })
    } catch (err) {
      setSave({
        row: 'prompt',
        kind: 'failed',
        message:
          err instanceof ApiError && err.status === 400
            ? err.message
            : "The prompt didn't save. Try again.",
      })
    }
  }

  const reset = async () => {
    const previous = doc.content
    setSave({ row: 'prompt', kind: 'busy' })
    try {
      const d = await api.promptReset(name)
      setDocs((all) => ({ ...all, [name]: d }))
      setEdits((all) => ({ ...all, [name]: undefined }))
      setSave(null)
      notify('Reset to default', {
        label: 'Undo',
        windowMs: UNDO_MS,
        run: () => {
          api
            .promptPut(name, previous)
            .then((back) => setDocs((all) => ({ ...all, [name]: back })))
            .catch(() => notify("The prompt didn't come back. Try again."))
        },
      })
    } catch {
      setSave({ row: 'prompt', kind: 'failed', message: "The prompt didn't reset. Try again." })
    } finally {
      setResets((n) => n + 1)
    }
  }

  return (
    <form className="set-fold-body" onSubmit={submit}>
      <fieldset className="set-prompt-rows" disabled={busy}>
        {picker}
        {doc.custom && <span className="set-badge">Customized</span>}
        <textarea
          ref={editor}
          className="mono set-prompt"
          aria-label={`${name} prompt`}
          rows={14}
          value={text}
          onChange={(e) => setText(e.target.value)}
        />
      </fieldset>
      <div className="set-acts">
        <button className="btn-haze small" disabled={!dirty || blank || busy}>
          {busy ? 'Saving…' : 'Save'}
        </button>
        <button
          type="button"
          className="set-link"
          disabled={(!doc.custom && !dirty) || busy}
          onClick={() => void reset()}
        >
          Reset to default
        </button>
        {blank && dirty && (
          <span className="pane-status bad" role="alert">
            A prompt can't be empty.
          </span>
        )}
        {!dirty && <Status save={save} row="prompt" />}
      </div>
    </form>
  )
}

function dayOf(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
}


const PASSKEY_HINT = 'Passkeys need an https address'

const passkeyLine = (k: Passkey) =>
  `Added ${dayOf(k.created_at)} · ${k.last_used_at ? `used ${dayOf(k.last_used_at)}` : 'never used'}`

// What the password prompt is standing in front of.
type Ask =
  | { kind: 'add-passkey' }
  | { kind: 'drop-passkey'; id: number }
  | { kind: 'start-totp' }
  | { kind: 'drop-totp' }

const ASK_LABEL: Record<Ask['kind'], string> = {
  'add-passkey': 'Add a passkey',
  'drop-passkey': 'Remove this passkey',
  'start-totp': 'Set up authenticator',
  'drop-totp': 'Remove the authenticator app',
}

function QrCode({ uri, label = 'Authenticator QR code' }: { uri: string; label?: string }) {
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

  if (!drawn) return <span className="set-sub">Use the manual key below.</span>
  return <canvas className="set-qr" ref={canvas} aria-label={label} />
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
        Open in your password manager
      </a>
      <span className="set-sub">Manual key</span>
      <code className="set-token-secret">{enrol.secret_base32}</code>
      <span className="set-sub">Enter a code from the app to finish</span>
      <div className="set-token-form">
        <input
          aria-label="Code from the app"
          inputMode="numeric"
          autoComplete="one-time-code"
          pattern="\d{6}"
          maxLength={6}
          value={code}
          onChange={(e) => setCode(e.target.value)}
        />
        <button type="submit" className="btn-haze small" disabled={busy || code.length !== 6}>
          Finish
        </button>
        <button type="button" className="set-link" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  )
}

function SecuritySection({ notify }: { notify: Notify }) {
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
      .then(setState)
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
      fail(err, "That didn't go through. Try again.")
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
      fail(err, "That passkey wasn't saved. Try again.")
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
      fail(err, "That name didn't save. Try again.")
    }
  }

  const confirmTotp = async (code: string) => {
    setBusy(true)
    try {
      await security.totpConfirm(code)
      setEnrol(null)
      load()
    } catch (err) {
      fail(err, "That code didn't match. Try the next one.")
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
      {arming === key ? 'Really remove?' : label}
    </button>
  )

  return (
    <div className="set-fold-body">
      <span className="set-sub">Used when the admin panel asks you to confirm it's you.</span>

      {state === 'error' && (
        <span className="set-sub">
          Security didn't load.{' '}
          <button className="set-link" onClick={load}>
            Retry
          </button>
        </span>
      )}

      {loaded?.passkeys.map((k) => (
        <div className="set-row set-token-row" key={k.id}>
          <span className="set-row-body">
            {rename?.id === k.id ? (
              <input
                aria-label="Passkey name"
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
          {removeButton(`passkey:${k.id}`, () => open({ kind: 'drop-passkey', id: k.id }), 'Remove')}
        </div>
      ))}

      {loaded && (
        <div className="set-row set-token-row">
          <span className="set-row-body">
            <span className="set-label">Authenticator app</span>
            <span className="set-sub">{loaded.totp.enabled ? 'on' : 'Any TOTP app or password manager'}</span>
          </span>
          {loaded.totp.enabled
            ? removeButton('totp', () => open({ kind: 'drop-totp' }), 'Remove')
            : !enrol && (
                <button
                  type="button"
                  className="btn-haze small"
                  disabled={busy}
                  onClick={() => open({ kind: 'start-totp' })}
                >
                  Set up authenticator
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
            Add a passkey
          </button>
          {!canPasskey && <span className="set-sub">{PASSKEY_HINT}</span>}
        </div>
      )}

      {ask && (
        <form className="set-token-form" onSubmit={(e) => void answer(e)}>
          <input
            type="password"
            aria-label={`Password to ${ASK_LABEL[ask.kind].toLowerCase()}`}
            placeholder="Your password"
            autoComplete="current-password"
            autoFocus
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
          <button type="submit" className="btn-haze small" disabled={busy || !password}>
            {ASK_LABEL[ask.kind]}
          </button>
          <button type="button" className="set-link" onClick={() => setAsk(null)}>
            Cancel
          </button>
        </form>
      )}

      {naming && (
        <form className="set-token-form" onSubmit={(e) => void keepPasskey(e)}>
          <input
            aria-label="Name this passkey"
            placeholder="Name this passkey"
            maxLength={64}
            autoFocus
            value={naming.name}
            onChange={(e) => setNaming({ ...naming, name: e.target.value })}
          />
          <button type="submit" className="btn-haze small" disabled={busy || !naming.name.trim()}>
            Save
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
          : "That token wasn't created. Try again.",
      )
    } finally {
      setBusy(false)
    }
  }

  const revoke = async (t: Token) => {
    if (arming !== t.id) {
      setArming(t.id)
      return
    }
    setArming(null)
    try {
      await api.revokeToken(t.id)
      setTokens((all) => (Array.isArray(all) ? all.filter((x) => x.id !== t.id) : all))
      if (fresh?.id === t.id) setFresh(null)
    } catch {
      notify("That token wasn't revoked. Try again.")
    }
  }

  return (
    <div className="set-fold-body">
      <form className="set-token-form" onSubmit={(e) => void create(e)}>
        <input
          aria-label="Token name"
          placeholder="Name this token"
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
          Create
        </button>
      </form>
      {fresh && (
        <div className="set-token-fresh">
          <code className="set-token-secret">{fresh.token}</code>
          <span className="set-sub">Copy it now. It won't be shown again.</span>
        </div>
      )}
      {tokens === 'error' && <span className="set-sub">Tokens didn't load.</span>}
      {Array.isArray(tokens) && tokens.length === 0 && (
        <span className="set-sub">No tokens yet.</span>
      )}
      {Array.isArray(tokens) &&
        tokens.map((t) => (
          <div className="set-row set-token-row" key={t.id}>
            <span className="set-row-body">
              <span className="set-label">{t.name}</span>
              <span className="set-sub">
                Created {dayOf(t.created_at)} ·{' '}
                {t.last_used_at ? `used ${dayOf(t.last_used_at)}` : 'never used'}
              </span>
            </span>
            <button
              type="button"
              className="btn-haze small"
              onClick={() => void revoke(t)}
              onBlur={() => setArming((a) => (a === t.id ? null : a))}
            >
              {arming === t.id ? 'Really revoke?' : 'Revoke'}
            </button>
          </div>
        ))}
    </div>
  )
}

const EXPIRIES = [
  { id: 7, label: '7 days' },
  { id: 30, label: '30 days' },
  { id: 120, label: '120 days' },
] as const

const DEFAULT_SCOPE: ShareScope = {
  today: true, tasks: true, categories: [], goals: true, progress: true,
  details: false, horizon_days: 3, notes: false, messages_per_day: 40,
}

function inDays(days: number): string {
  return new Date(Date.now() + days * 24 * 60 * 60 * 1000).toISOString()
}

function scopeWords(s: ShareScope): string {
  const parts: string[] = []
  if (s.tasks) parts.push(s.categories.length > 0 ? s.categories.join(', ') : 'tasks')
  if (s.today) parts.push('plan')
  if (s.goals) parts.push('goals')
  if (s.progress) parts.push('progress')
  const words = parts.join(', ') || 'nothing'
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
      <input aria-label="Link name" placeholder="Who is this for" maxLength={64} value={d.name} onChange={(e) => setD({ ...d, name: e.target.value })} />
      {expiryNote && <span className="set-sub">{expiryNote}</span>}
      <div className="set-row set-share-expiry">
        <span className="set-row-body"><span className="set-label">Ends after</span></span>
        <div className="seg" role="group" aria-label="Expiry">
          {EXPIRIES.map((x) => (
            <button key={x.id} type="button" aria-pressed={d.days === x.id && d.date === ''} onClick={() => setD({ ...d, days: x.id, date: '' })}>{x.label}</button>
          ))}
        </div>
      </div>
      <div className="set-row">
        <span className="set-row-body"><span className="set-label">Or ends on a date</span></span>
        <input type="date" aria-label="Ends on a date" min={localToday()} value={d.date} onChange={(e) => setD({ ...d, date: e.target.value })} />
      </div>
      <div className="set-row"><span className="set-row-body"><span className="set-label">The plan for the next days</span></span><Switch label="Share the plan" on={d.scope.today} onToggle={() => scope({ today: !d.scope.today })} /></div>
      {d.scope.today && (
        <div className="set-row"><span className="set-row-body"><span className="set-label">How many days ahead</span></span><input type="number" aria-label="How many days ahead" min={1} max={14} value={d.scope.horizon_days} onChange={(e) => scope({ horizon_days: clampedOr(e.target.value, d.scope.horizon_days, 1, 14) })} /></div>
      )}
      <div className="set-row"><span className="set-row-body"><span className="set-label">Open tasks</span></span><Switch label="Share tasks" on={d.scope.tasks} onToggle={() => scope({ tasks: !d.scope.tasks })} /></div>
      {d.scope.tasks && (
        <div className="set-row"><span className="set-row-body"><span className="set-label">Task descriptions and notes</span></span><Switch label="Share details" on={d.scope.details} onToggle={() => scope({ details: !d.scope.details })} /></div>
      )}
      <div className="set-row"><span className="set-row-body"><span className="set-label">Goals</span></span><Switch label="Share goals" on={d.scope.goals} onToggle={() => scope({ goals: !d.scope.goals })} /></div>
      <div className="set-row"><span className="set-row-body"><span className="set-label">Done this week</span></span><Switch label="Share progress" on={d.scope.progress} onToggle={() => scope({ progress: !d.scope.progress })} /></div>
      {filtered && pills.length > 0 && (
        <div className="set-share-cats">
          <span className="set-sub">Only these categories, or none for all</span>
          <div className="seg wrap" role="group" aria-label="Categories">
            {pills.map((c) => (
              <button key={c} type="button" aria-pressed={d.scope.categories.includes(c)} onClick={() => toggleCategory(c)}>{c}</button>
            ))}
          </div>
        </div>
      )}
      <div className="set-row"><span className="set-row-body"><span className="set-label">They can leave you a note</span></span><Switch label="Allow notes" on={d.scope.notes} onToggle={() => scope({ notes: !d.scope.notes })} /></div>
      <div className="set-row"><span className="set-row-body"><span className="set-label">Messages a day</span></span><input type="number" aria-label="Messages a day" min={1} max={100} value={d.scope.messages_per_day} onChange={(e) => scope({ messages_per_day: clampedOr(e.target.value, d.scope.messages_per_day, 1, 100) })} /></div>
      <textarea aria-label="Brief for Note" placeholder="Tell Note how to talk to them and what to steer clear of" rows={3} maxLength={4096} value={d.brief} onChange={(e) => setD({ ...d, brief: e.target.value })} />
      <span className="set-sub">Note reads this before every reply on this link.</span>
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
  const [busy, setBusy] = useState(false)
  const [fresh, setFresh] = useState<Share | null>(null)
  const threadsFor = useRef<number | null>(null)

  useEffect(() => {
    api.shares().then(setShares).catch(() => setShares('error'))
    api
      .tasks()
      .then((ts) => setCategories([...new Set(ts.map((t) => t.category).filter((c) => c !== ''))].sort()))
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
      notify(err instanceof ApiError && (err.status === 409 || err.status === 422) ? err.message : "That link wasn't created. Try again.")
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
      notify('Saved')
    } catch (err) {
      notify(err instanceof ApiError && err.status === 422 ? err.message : "That change wasn't saved. Try again.")
    } finally {
      setBusy(false)
    }
  }

  const doRevoke = async (s: Share) => {
    try {
      await api.revokeShare(s.id)
      setShares((all) => (Array.isArray(all) ? all.filter((x) => x.id !== s.id) : all))
      if (fresh?.id === s.id) setFresh(null)
      notify('Link revoked')
    } catch {
      notify("That link wasn't revoked. Try again.")
    }
  }

  const copy = (s: Share) => {
    const fallback = () => notify('Copy the link from the box above')
    if (navigator.clipboard) navigator.clipboard.writeText(s.url).then(() => notify('Link copied'), fallback)
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
    const land = (ts: ShareThread[]) => {
      if (threadsFor.current === s.id) setThreads(ts)
    }
    api.shareThreads(s.id).then(land).catch(() => land([]))
  }

  const blank: ShareDraft = { name: '', brief: '', scope: DEFAULT_SCOPE, days: 30, date: '' }
  const draftOf = (s: Share): ShareDraft => ({ name: s.name, brief: s.brief, scope: s.scope, days: 0, date: '' })

  return (
    <div className="set-fold-body">
      <span className="set-sub">A link lets someone you trust chat with Note about part of your day. The switches say what Note may look at for them.</span>
      {!creating && <button type="button" className="btn-haze small" onClick={() => setCreating(true)}>New link</button>}
      {creating && <ShareForm initial={blank} categories={categories} submit={(d) => void create(d)} busy={busy} label="Create link" />}
      {fresh && (
        <div className="set-token-fresh">
          <code className="set-token-secret">{fresh.url}</code>
          <span className="set-sub">Anyone with this link sees what {fresh.name} was given until {dayOf(fresh.expires_at)}.</span>
        </div>
      )}
      {shares === 'error' && <span className="set-sub">Links didn't load.</span>}
      {Array.isArray(shares) && shares.length === 0 && !creating && <span className="set-sub">No links yet.</span>}
      {Array.isArray(shares) &&
        shares.map((s) => (
          <div key={s.id} className="set-share">
            <div className="set-row set-token-row">
              <span className="set-row-body">
                <span className="set-label">{s.name}</span>
                <span className="set-sub">
                  {scopeWords(s.scope)}, until {dayOf(s.expires_at)}, {s.messages_today} {s.messages_today === 1 ? 'message' : 'messages'} today
                </span>
              </span>
              <Overflow
                className="set-share-more"
                label={`More for ${s.name}`}
                items={[
                  { label: 'Copy link', run: () => copy(s) },
                  { label: 'Preview as visitor', run: () => window.open(s.url, '_blank', 'noopener') },
                  { label: openThreads === s.id ? 'Hide conversations' : 'Conversations', run: () => showThreads(s) },
                  { label: editing === s.id ? 'Stop editing' : 'Edit', run: () => setEditing(editing === s.id ? null : s.id) },
                  { label: 'Revoke', kind: 'danger', run: () => notify('Revoke this link?', { label: 'Revoke', run: () => void doRevoke(s) }) },
                ]}
              />
            </div>
            {editing === s.id && (
              <ShareForm
                initial={draftOf(s)}
                categories={categories}
                submit={(d) => void save(s, d)}
                busy={busy}
                label="Save"
                expiryNote={`Currently until ${dayOf(s.expires_at)}; pick a preset or a date to change it`}
              />
            )}
            {openThreads === s.id && (
              <div className="set-share-threads">
                {threads === undefined && <span className="set-sub">Loading</span>}
                {threads && threads.length === 0 && <span className="set-sub">No one has asked anything yet.</span>}
                {threads?.map((t) => (
                  <div key={t.id} className="set-share-thread">
                    <span className="set-sub">Visitor from {dayOf(t.created_at)}, last {dayOf(t.updated_at)}</span>
                    {t.messages.map((m, i) => (
                      <p key={i} className={`set-share-msg ${m.role}`}>
                        {m.role === 'note' ? 'Note for you: ' : m.role === 'user' ? 'They: ' : 'Note: '}
                        {m.content}
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
        notify('Web Push is not configured on this server.')
      } else {
        notify("Notifications didn't change. Try again.")
      }
      setState(was)
    }
  }

  return (
    <div className="set-row">
      <span className="set-row-body">
        <span className="set-label">Push on this phone</span>
        {state === 'unsupported' && <span className="set-sub">Not on this browser</span>}
      </span>
      <Switch
        label="Push on this phone"
        on={state === 'on'}
        disabled={state === 'busy' || state === 'unsupported'}
        onToggle={() => void toggle()}
      />
    </div>
  )
}
