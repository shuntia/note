import { useEffect, useState, type FormEvent } from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { disablePush, enablePush, pushState } from '../push'
import { SectionTitle } from '../section'
import { applyTheme, saveTheme, storedTheme, type ThemeChoice } from '../theme'
import type { LogRow, Me, Settings } from '../types'

export function More({
  me,
  notify,
  onSignedOut,
}: ViewProps & { me: Me; onSignedOut: () => void }) {
  return (
    <div className="panes">
      <SettingsCard />
      <AppearanceCard />
      <PushCard notify={notify} />
      {me.admin && <AdminCard />}
      <section className="card pane">
        <SectionTitle>Session</SectionTitle>
        <p className="pane-note muted">Signed in as {me.username}.</p>
        <button
          className="quiet danger"
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
      </section>
    </div>
  )
}

const EDITABLE = ['display_name', 'timezone', 'nightly_time', 'template'] as const

type Draft = Pick<Settings, (typeof EDITABLE)[number]>
type Choices = Pick<Settings, 'templates' | 'timezones'>
type Loaded = { choices: Choices; baseline: Draft; draft: Draft }
type SaveState = { kind: 'idle' | 'busy' | 'saved' } | { kind: 'failed'; message: string }

function draftOf(s: Settings): Draft {
  return {
    display_name: s.display_name,
    timezone: s.timezone,
    nightly_time: s.nightly_time,
    template: s.template,
  }
}

function SettingsCard() {
  const [state, setState] = useState<Loaded | 'error' | undefined>(undefined)
  const [save, setSave] = useState<SaveState>({ kind: 'idle' })

  const load = () => {
    setState(undefined)
    setSave({ kind: 'idle' })
    api
      .settings()
      .then((s) =>
        setState({
          choices: { templates: s.templates, timezones: s.timezones },
          baseline: draftOf(s),
          draft: draftOf(s),
        }),
      )
      .catch(() => setState('error'))
  }
  useEffect(load, [])

  const edit = <K extends keyof Draft>(key: K, value: Draft[K]) => {
    if (save.kind === 'busy') return
    setSave({ kind: 'idle' })
    setState((s) => (s && s !== 'error' ? { ...s, draft: { ...s.draft, [key]: value } } : s))
  }

  if (state === undefined) {
    return (
      <section className="card pane">
        <SectionTitle>Settings</SectionTitle>
        <p className="muted">Loading…</p>
      </section>
    )
  }
  if (state === 'error') {
    return (
      <section className="card pane">
        <SectionTitle>Settings</SectionTitle>
        <p className="muted">
          Settings didn't load.{' '}
          <button className="quiet" onClick={load}>
            Retry
          </button>
        </p>
      </section>
    )
  }

  const { choices, baseline, draft } = state
  const cleaned: Draft = { ...draft, display_name: draft.display_name.trim() }
  const patch: Partial<Draft> = {}
  for (const key of EDITABLE) if (cleaned[key] !== baseline[key]) patch[key] = cleaned[key]
  const dirty = Object.keys(patch).length > 0

  // A template dropped from the config directory would otherwise select as blank.
  const templates = choices.templates.includes(draft.template)
    ? choices.templates
    : [draft.template, ...choices.templates]

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    if (!dirty) return
    setSave({ kind: 'busy' })
    try {
      await api.saveSettings(patch)
      // The saved values become the baseline, but any field edited during the flight
      // keeps what the user typed rather than snapping back to the submitted value.
      setState((s) => {
        if (!s || s === 'error') return s
        const merged = { ...s.draft }
        for (const key of EDITABLE) if (s.draft[key] === draft[key]) merged[key] = cleaned[key]
        return { ...s, baseline: cleaned, draft: merged }
      })
      setSave({ kind: 'saved' })
    } catch (err) {
      setSave({
        kind: 'failed',
        message:
          err instanceof ApiError && err.status === 400
            ? err.message
            : "Settings didn't save. Try again.",
      })
    }
  }

  return (
    <section className="card pane">
      <SectionTitle>Settings</SectionTitle>
      <form onSubmit={submit}>
        <fieldset className="pane-rows" disabled={save.kind === 'busy'}>
          <div className="pane-row">
            <label className="pane-label" htmlFor="set-name">
              Display name
            </label>
            <input
              id="set-name"
              value={draft.display_name}
              autoComplete="name"
              onChange={(e) => edit('display_name', e.target.value)}
            />
          </div>

          <div className="pane-row">
            <label className="pane-label" htmlFor="set-tz">
              Timezone
            </label>
            <input
              id="set-tz"
              className="mono"
              list="tz-list"
              spellCheck={false}
              autoCapitalize="none"
              value={draft.timezone}
              onChange={(e) => edit('timezone', e.target.value)}
            />
            <datalist id="tz-list">
              {choices.timezones.map((tz) => (
                <option key={tz} value={tz} />
              ))}
            </datalist>
            <p className="pane-hint">Type to search, or enter any IANA zone name.</p>
          </div>

          <div className="pane-row inline">
            <label className="pane-label" htmlFor="set-time">
              Nightly debrief
            </label>
            <input
              id="set-time"
              className="mono"
              type="time"
              value={draft.nightly_time}
              onChange={(e) => edit('nightly_time', e.target.value)}
            />
          </div>

          <div className="pane-row inline">
            <label className="pane-label" htmlFor="set-template">
              Template
            </label>
            <select
              id="set-template"
              value={draft.template}
              onChange={(e) => edit('template', e.target.value)}
            >
              {templates.map((t) => (
                <option key={t} value={t}>
                  {t}
                </option>
              ))}
            </select>
          </div>
        </fieldset>

        <div className="pane-foot">
          <button className="primary" disabled={!dirty || save.kind === 'busy'}>
            {save.kind === 'busy' ? 'Saving…' : 'Save changes'}
          </button>
          {save.kind === 'saved' && (
            <span className="pane-status ok" role="status">
              Saved.
            </span>
          )}
          {save.kind === 'failed' && (
            <span className="pane-status bad" role="alert">
              {save.message}
            </span>
          )}
        </div>
      </form>
    </section>
  )
}

const THEMES: { id: ThemeChoice; label: string }[] = [
  { id: 'system', label: 'System' },
  { id: 'light', label: 'Light' },
  { id: 'dark', label: 'Dark' },
]

function AppearanceCard() {
  const [theme, setTheme] = useState<ThemeChoice>(storedTheme)

  const choose = (choice: ThemeChoice) => {
    setTheme(choice)
    applyTheme(choice)
    saveTheme(choice)
  }

  return (
    <section className="card pane">
      <SectionTitle>Appearance</SectionTitle>
      <div className="pane-rows">
        <div className="pane-row inline">
          <span className="pane-label" id="theme-label">
            Theme
          </span>
          <div className="seg" role="group" aria-labelledby="theme-label">
            {THEMES.map((t) => (
              <button
                key={t.id}
                type="button"
                aria-pressed={theme === t.id}
                onClick={() => choose(t.id)}
              >
                {t.label}
              </button>
            ))}
          </div>
        </div>
      </div>
    </section>
  )
}

function PushCard({ notify }: { notify: (msg: string) => void }) {
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

  if (state === 'unsupported') return null
  return (
    <section className="card pane">
      <SectionTitle>Notifications</SectionTitle>
      <div className="pane-rows">
        <div className="pane-row inline">
          <span className="pane-label">
            Push to this device
            <span className="pane-hint">{state === 'on' ? 'On' : 'Off'}</span>
          </span>
          <button className="quiet" disabled={state === 'busy'} onClick={toggle}>
            {state === 'on' ? 'Turn off' : 'Turn on'}
          </button>
        </div>
      </div>
    </section>
  )
}

function AdminCard() {
  const [log, setLog] = useState<LogRow[] | 'error' | undefined>(undefined)

  const load = () => {
    setLog(undefined)
    api
      .adminLog()
      .then(setLog)
      .catch(() => setLog('error'))
  }
  useEffect(load, [])

  return (
    <section className="card pane">
      <SectionTitle>Server log</SectionTitle>
      {log === undefined ? (
        <p className="muted">Loading…</p>
      ) : log === 'error' ? (
        <p className="muted">
          The log didn't load.{' '}
          <button className="quiet" onClick={load}>
            Retry
          </button>
        </p>
      ) : log.length === 0 ? (
        <p className="muted">Nothing logged yet.</p>
      ) : (
        <div className="log-scroll">
          <table className="log-table">
            <tbody>
              {log.map((row, i) => (
                <tr key={i}>
                  <td className="mono">{row.ts.slice(11, 19)}</td>
                  <td>{row.kind}</td>
                  <td className="muted">{row.detail}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  )
}
