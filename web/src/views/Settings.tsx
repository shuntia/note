import { useEffect, useState } from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { desktop } from '../desktop'
import { useLoad, useSave } from '../hooks'
import { adoptLanguage, t, type Key } from '../i18n'
import { prefsFrom, writePrefs } from '../prefs'
import type { CounterMode } from '../session'
import '../styles/settings.css'
import { applyTheme, paintSky, savePlace, saveTheme, storedPlace, storedTheme, type ThemeChoice } from '../theme'
import type { Language, Me, ScheduleRow, Settings as UserSettings, VoiceChoice } from '../types'
import { Chevron, FoldRow, Group, Status, ToggleRow } from '../ui/rows'
import { previewPlayer } from '../voicePreview'
import { VoiceSheet } from '../voiceSheet'
import { deviceZone, knownDeviceZone } from '../zone'
import { AboutSection } from './settings/AboutSection'
import { ComputerGroup, PushRow } from './settings/ComputerGroup'
import { DebugSection } from './settings/DebugSection'
import { PasswordSection } from './settings/PasswordSection'
import { RING_CHOICES, RingChoices } from './settings/RingChoices'
import { ScheduleList } from './settings/ScheduleList'
import { SecuritySection } from './settings/SecuritySection'
import { SharesSection } from './settings/SharesSection'
import { SkyStrip } from './settings/SkyStrip'
import { SettingsContext, ToggleSetting, type SaveSettings } from './settings/ToggleSetting'
import { TokensSection } from './settings/TokensSection'
import { MAX_TRIGGERS_PER_DAY, triggersFromInput, triggersLabel, UNLIMITED } from './settings/triggers'

export { RingChoices, SecuritySection }

const EDITABLE = [
  'display_name',
  'timezone',
  'nightly_time',
  'close_day_time',
  'calls_until',
  'morning_until',
  'template',
  'triggers_per_day',
  'pomodoro_work_min',
  'pomodoro_break_min',
  'idle_nudge_min',
] as const

const MXID_EXAMPLE = '@you:server'
const WORK_MIN = { min: 5, max: 120 }
const BREAK_MIN = { min: 1, max: 60 }
const IDLE_MIN = { min: 0, max: 240 }

type Draft = Pick<UserSettings, (typeof EDITABLE)[number]>
// `settings` is the server's last word; `draft` holds the text fields as they are typed.
type Loaded = { settings: UserSettings; draft: Draft }

const LANGUAGES: { id: Language; key: Key }[] = [
  { id: '', key: 'settings.language.auto' },
  { id: 'en', key: 'settings.language.en' },
  { id: 'ja', key: 'settings.language.ja' },
]

const THEMES: { id: ThemeChoice; key: Key }[] = [
  { id: 'system', key: 'settings.theme.system' },
  { id: 'light', key: 'settings.theme.light' },
  { id: 'dark', key: 'settings.theme.dark' },
  { id: 'sky', key: 'settings.theme.sky' },
]

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
    calls_until: s.calls_until,
    morning_until: s.morning_until,
    template: s.template,
    triggers_per_day: s.triggers_per_day,
    pomodoro_work_min: s.pomodoro_work_min,
    pomodoro_break_min: s.pomodoro_break_min,
    idle_nudge_min: s.idle_nudge_min,
  }
}

const templateChoices = ({ settings, draft }: Loaded) =>
  settings.templates.includes(draft.template) ? settings.templates : [draft.template, ...settings.templates]

// A zone reads as the city it names; the region is the same for every choice nearby.
const zoneCity = (tz: string) => tz.split('/').pop()?.replace(/_/g, ' ') ?? tz

const failure = (err: unknown) =>
  err instanceof ApiError && (err.status === 400 || err.status === 422) ? err.message : t('settings.saveFailed')

const callFailure = (err: unknown) =>
  err instanceof ApiError && [400, 409, 422, 503].includes(err.status) ? err.message : t('settings.notThrough')

export function Settings({
  me,
  notify,
  onChanged,
  onSignedOut,
  openAdmin,
}: ViewProps & { me: Me; onSignedOut: () => void; openAdmin: () => void }) {
  const [state, reload, setState] = useLoad(
    () => api.settings().then((s): Loaded => ({ settings: s, draft: draftOf(s) })),
    [],
    { blank: true },
  )
  const { save, setSave, busy, run } = useSave()
  const [open, setOpen] = useState<string | null>(null)
  const [theme, setTheme] = useState<ThemeChoice>(storedTheme)
  const [place, setPlace] = useState(storedPlace)
  const [mxid, setMxid] = useState('')

  const loaded = state !== undefined && state !== 'error' ? state : null
  const settings = loaded?.settings

  const fold = (id: string) => () => setOpen((o) => (o === id ? null : id))

  const update = (next: (s: Loaded) => Loaded) => setState((s) => (s && s !== 'error' ? next(s) : s))
  const land = (patch: Partial<UserSettings>) => update((s) => ({ ...s, settings: { ...s.settings, ...patch } }))
  const edit = <K extends keyof Draft>(key: K, value: Draft[K]) =>
    update((s) => ({ ...s, draft: { ...s.draft, [key]: value } }))

  const saveSettings: SaveSettings = async (row, patch, { alerts, quiet, after } = {}) => {
    setSave({ row, kind: 'busy' })
    try {
      const saved = await api.saveSettings(patch, alerts)
      if (after?.(saved)) return true
      land(saved)
      writePrefs(prefsFrom(saved))
      setSave(quiet ? null : { row, kind: 'saved' })
      return true
    } catch (err) {
      setSave({ row, kind: 'failed', message: failure(err) })
      return false
    }
  }

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
      if (cleaned[key] !== loaded.settings[key]) Object.assign(patch, { [key]: cleaned[key] })
    }
    if (Object.keys(patch).length === 0) return
    await saveSettings(row, patch, { after: () => void update((s) => ({ ...s, draft: cleaned })) })
  }

  const chooseLanguage = (language: Language) =>
    saveSettings(
      'language',
      { language },
      {
        after: (saved) => {
          if (!adoptLanguage(saved.language)) return false
          location.reload()
          return true
        },
      },
    )

  // Turning the switch on moves the day to this device at once rather than on the
  // next focus.
  const commitZoneAuto = async (on: boolean) => {
    if (!loaded) return
    const device = deviceZone()
    const follow = on && knownDeviceZone(device, loaded.settings.timezones)
    await saveSettings('timezone', follow ? { timezone_auto: on, timezone: device } : { timezone_auto: on }, {
      after: (saved) => {
        update((s) => ({ ...s, draft: { ...s.draft, timezone: saved.timezone } }))
        onChanged()
      },
    })
  }

  const toggleAlert = async (row: ScheduleRow) => {
    const next = !row.alert
    const setAlert = (value: boolean) =>
      update((s) => ({
        ...s,
        settings: {
          ...s.settings,
          schedule: s.settings.schedule.map((r) => (r.index === row.index ? { ...r, alert: value } : r)),
        },
      }))
    setAlert(next)
    if (!(await saveSettings('schedule', {}, { alerts: [{ index: row.index, alert: next }] }))) setAlert(row.alert)
  }

  const linkVoice = () =>
    run(
      async () => {
        const got = await api.voiceLink(mxid.trim())
        land({ voice_link: { mxid: got.mxid, state: 'invited' } })
        setMxid('')
        return null
      },
      callFailure,
      'matrix',
    )

  const unlinkVoice = () =>
    run(
      async () => {
        await api.voiceUnlink()
        land({ voice_link: null })
      },
      callFailure,
      'matrix',
    )

  const ringNow = () =>
    run(
      async () => {
        await api.voiceTest()
        return t('settings.ringing')
      },
      callFailure,
      'calls',
    )

  const sendTest = () =>
    run(
      async () => t('settings.test.sent', { via: (await api.notifyTest()).via }),
      () => t('settings.test.failed'),
      'test',
    )

  const [voices, setVoices] = useState<VoiceChoice[] | null>(null)
  const [preview] = useState(() => previewPlayer())
  useEffect(() => () => preview.stop(), [preview])
  const callsLinked = open === 'calls' && settings?.voice_link?.state === 'linked'
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

  // Lands at once and steps back only if the save fails and nothing newer was picked.
  const [choosingVoice, setChoosingVoice] = useState(false)
  const pickVoice = async (voice: string) => {
    const before = settings?.voice_voice ?? ''
    land({ voice_voice: voice })
    try {
      await api.saveSettings({ voice_voice: voice })
    } catch (err) {
      update((s) =>
        s.settings.voice_voice === voice ? { ...s, settings: { ...s.settings, voice_voice: before } } : s,
      )
      setSave({ row: 'calls', kind: 'failed', message: failure(err) })
    }
  }

  // The invite is accepted over in Element, so the row watches for it.
  const invited = settings?.voice_link?.state === 'invited'
  useEffect(() => {
    if (!invited) return
    const id = window.setInterval(() => {
      void api
        .settings()
        .then((s) => {
          const next = s.voice_link
          update((prev) =>
            prev.settings.voice_link?.mxid !== next?.mxid || prev.settings.voice_link?.state !== next?.state
              ? { ...prev, settings: { ...prev.settings, voice_link: next } }
              : prev,
          )
        })
        .catch(() => undefined)
    }, 3000)
    return () => window.clearInterval(id)
  }, [invited])

  const chooseTheme = (choice: ThemeChoice) => {
    setTheme(choice)
    applyTheme(choice)
    saveTheme(choice)
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

  const themeLabel = t(THEMES.find((x) => x.id === theme)?.key ?? 'settings.theme.system')
  const counterLabel = t(COUNTERS.find((c) => c.id === settings?.counter)?.key ?? 'settings.counter.remaining')

  return (
    <SettingsContext.Provider value={settings ? { settings, save, busy, saveSettings } : null}>
      <div className="settings">
        {state === 'error' && (
          <p className="set-sub">
            {t('settings.loadFailed')}{' '}
            <button
              className="set-link"
              onClick={() => {
                setSave(null)
                reload()
              }}
            >
              {t('common.retry')}
            </button>
          </p>
        )}

        {loaded && settings && (
          <Group head={t('settings.sessions')}>
            <FoldRow
              label={t('settings.pomodoro')}
              value={
                settings.pomodoro_enabled
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
                  <ToggleSetting
                    field="pomodoro_enabled"
                    label={t('settings.pomodoro.rounds')}
                    aria={t('settings.pomodoro')}
                    row="pomodoro"
                  />
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
            <ToggleSetting field="session_end_notify" label={t('settings.notifyEnd')} />
            <ToggleSetting field="overrun_wakes" label={t('settings.overrunWakes')} />
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
                        aria-pressed={settings.counter === c.id}
                        onClick={() => void saveSettings('counter', { counter: c.id })}
                      >
                        {t(c.key)}
                      </button>
                    ))}
                  </div>
                  <Status save={save} row="counter" />
                </div>
              )}
            </FoldRow>
            <ToggleSetting field="show_arc_between_sessions" label={t('settings.arc')} />
          </Group>
        )}

        {loaded && settings && (
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
                  <ScheduleList rows={settings.schedule} busy={busy} toggle={toggleAlert} />
                  <Status save={save} row="schedule" />
                </div>
              )}
            </FoldRow>
            <FoldRow
              label={t('settings.nightly')}
              value={
                settings.nightly_enabled
                  ? t('settings.nightly.on', { time: loaded.draft.nightly_time })
                  : t('settings.off')
              }
              open={open === 'nightly'}
              onToggle={fold('nightly')}
            >
              {open === 'nightly' && (
                <div className="set-fold-body">
                  <ToggleSetting
                    field="nightly_enabled"
                    label={t('settings.nightly.sub')}
                    aria={t('settings.nightly')}
                  />
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
              label={t('settings.callsUntil')}
              value={loaded.draft.calls_until || t('settings.callsUntil.midnight')}
              open={open === 'calls_until'}
              onToggle={fold('calls_until')}
            >
              {open === 'calls_until' && (
                <div className="set-fold-body">
                  <span className="set-sub">{t('settings.callsUntil.sub')}</span>
                  <input
                    type="time"
                    aria-label={t('settings.callsUntil')}
                    value={loaded.draft.calls_until}
                    onChange={(e) => edit('calls_until', e.target.value)}
                    {...commitOn('calls_until')}
                  />
                  <Status save={save} row="calls_until" />
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
                  <ToggleRow
                    label={t('settings.followDevice')}
                    on={settings.timezone_auto}
                    disabled={busy}
                    onToggle={() => void commitZoneAuto(!settings.timezone_auto)}
                  />
                  {settings.timezone_auto ? (
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
                    {settings.timezones.map((tz) => (
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
          <ToggleSetting field="checkins_enabled" label={t('settings.checkins')} sub={t('settings.checkins.sub')} />
          {loaded && (
            <FoldRow
              label={t('settings.triggers')}
              value={triggersLabel(loaded.draft.triggers_per_day)}
              open={open === 'triggers'}
              onToggle={fold('triggers')}
            >
              {open === 'triggers' && (
                <div className="set-fold-body">
                  <span className="set-sub">{t('settings.triggers.sub')}</span>
                  <input
                    aria-label={t('settings.triggers.input')}
                    type="number"
                    min={UNLIMITED}
                    max={MAX_TRIGGERS_PER_DAY}
                    placeholder="∞"
                    value={loaded.draft.triggers_per_day === UNLIMITED ? '' : loaded.draft.triggers_per_day}
                    onChange={(e) => edit('triggers_per_day', triggersFromInput(e.target.value))}
                    {...commitOn('triggers')}
                  />
                  <Status save={save} row="triggers" />
                  <div className="set-acts">
                    {loaded.draft.triggers_per_day !== UNLIMITED && (
                      <button
                        type="button"
                        className="set-link"
                        aria-label={t('settings.triggers.unlimited')}
                        disabled={busy}
                        onClick={() => {
                          edit('triggers_per_day', UNLIMITED)
                          void commit('triggers', { triggers_per_day: UNLIMITED })
                        }}
                      >
                        ∞
                      </button>
                    )}
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
          <ToggleSetting field="show_triggers" label={t('settings.showTriggers')} />
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

        {settings?.voice_enabled && (
          <Group head={t('settings.connections')}>
            <FoldRow
              label={t('settings.matrix')}
              value={
                settings.voice_link?.state === 'linked'
                  ? settings.voice_link.mxid
                  : settings.voice_link
                    ? t('settings.matrix.invited')
                    : t('settings.matrix.notLinked')
              }
              open={open === 'matrix'}
              onToggle={fold('matrix')}
            >
              {open === 'matrix' && (
                <div className="set-fold-body">
                  {settings.voice_link?.state === 'linked' ? (
                    <>
                      {settings.matrix_enabled && (
                        <>
                          <ToggleSetting field="matrix_send" label={t('settings.matrix.send')} row="matrix" />
                          {settings.matrix_send && (
                            <ToggleSetting field="matrix_ping" label={t('settings.matrix.ping')} row="matrix" />
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
                  ) : settings.voice_link ? (
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
            {settings.voice_link?.state === 'linked' && (
              <FoldRow
                label={t('settings.calls')}
                value={t((RING_CHOICES.find((c) => c.id === settings.ring_for) ?? RING_CHOICES[0]).key)}
                open={open === 'calls'}
                onToggle={fold('calls')}
              >
                {open === 'calls' && (
                  <div className="set-fold-body">
                    <RingChoices
                      value={settings.ring_for}
                      disabled={busy}
                      onPick={(v) => void saveSettings('calls', { ring_for: v })}
                    />
                    {voices && voices.length > 0 && (
                      <button type="button" className="set-row set-row-button" onClick={() => setChoosingVoice(true)}>
                        <span className="set-row-body">
                          <span className="set-label">{t('settings.voice')}</span>
                        </span>
                        <span className="set-value">
                          {(voices.find((v) => v.id === settings.voice_voice) ?? voices[0]).label}
                        </span>
                      </button>
                    )}
                    {choosingVoice && voices && (
                      <VoiceSheet
                        voices={voices}
                        chosen={settings.voice_voice || voices[0].id}
                        preview={preview}
                        onPick={(v) => void pickVoice(v)}
                        onClose={() => setChoosingVoice(false)}
                      />
                    )}
                    <ToggleSetting field="voice_cue" label={t('settings.sounds')} row="calls" quiet />
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
          {settings && (
            <FoldRow
              label={t('settings.language')}
              value={t(LANGUAGES.find((x) => x.id === settings.language)?.key ?? 'settings.language.auto')}
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
                        aria-pressed={settings.language === x.id}
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

        {desktop() && <ComputerGroup />}

        {me.admin && (
          <Group head={t('settings.admin')}>
            <button className="set-row set-open" onClick={openAdmin}>
              <span className="set-row-body">
                <span className="set-label">{t('settings.adminPanel')}</span>
              </span>
              <Chevron />
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
    </SettingsContext.Provider>
  )
}
