import { useEffect, useRef, useState, type FormEvent, type ReactNode } from 'react'
import { api, ApiError } from '../api'
import type { ToastAction, ViewProps } from '../app'
import { prefsFrom, writePrefs } from '../prefs'
import { disablePush, enablePush, pushState } from '../push'
import { eventLabel } from '../receipts'
import type { CounterMode } from '../session'
import { applyTheme, saveTheme, storedTheme, type ThemeChoice } from '../theme'
import type {
  Me,
  PromptDoc,
  PromptName,
  ScheduleRow,
  Settings as UserSettings,
  Token,
  TokenCreated,
} from '../types'

type Notify = (msg: string, action?: ToastAction) => void

const EDITABLE = ['display_name', 'timezone', 'nightly_time', 'template'] as const

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
}
type Save = { row: string; kind: 'busy' | 'saved' | 'failed'; message?: string } | null

const THEMES: { id: ThemeChoice; label: string }[] = [
  { id: 'system', label: 'System' },
  { id: 'light', label: 'Light' },
  { id: 'dark', label: 'Dark' },
]

const COUNTERS: { id: CounterMode; label: string }[] = [
  { id: 'remaining', label: 'Remaining' },
  { id: 'elapsed', label: 'Elapsed' },
]

function draftOf(s: UserSettings): Draft {
  return {
    display_name: s.display_name,
    timezone: s.timezone,
    nightly_time: s.nightly_time,
    template: s.template,
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
        ✓ Saved
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

  const load = () => {
    setState(undefined)
    setSave(null)
    api
      .settings()
      .then((s) =>
        setState({
          choices: { templates: s.templates, timezones: s.timezones },
          baseline: draftOf(s),
          draft: draftOf(s),
          rows: s.schedule,
          arc: s.show_arc_between_sessions,
          counter: s.counter,
          nightly: s.nightly_enabled,
          checkins: s.checkins_enabled,
        }),
      )
      .catch(() => setState('error'))
  }
  useEffect(load, [])

  const loaded = state !== undefined && state !== 'error' ? state : null

  const fold = (id: string) => () => setOpen((o) => (o === id ? null : id))

  const edit = <K extends keyof Draft>(key: K, value: Draft[K]) =>
    setState((s) => (s && s !== 'error' ? { ...s, draft: { ...s.draft, [key]: value } } : s))

  const failure = (err: unknown) =>
    err instanceof ApiError && err.status === 400 ? err.message : "That didn't save. Try again."

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
    for (const key of EDITABLE) if (cleaned[key] !== loaded.baseline[key]) patch[key] = cleaned[key]
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
    patch: { nightly_enabled?: boolean; checkins_enabled?: boolean },
  ) => {
    setSave({ row, kind: 'busy' })
    try {
      const saved = await api.saveSettings(patch)
      setState((s) =>
        s && s !== 'error'
          ? { ...s, nightly: saved.nightly_enabled, checkins: saved.checkins_enabled }
          : s,
      )
      setSave({ row, kind: 'saved' })
    } catch (err) {
      setSave({ row, kind: 'failed', message: failure(err) })
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

  const commitOn = (row: string) => ({
    onBlur: () => void commit(row),
    onKeyDown: (e: { key: string; currentTarget: HTMLInputElement }) => {
      if (e.key === 'Enter') e.currentTarget.blur()
    },
  })

  const busy = save?.kind === 'busy'
  const themeLabel = THEMES.find((t) => t.id === theme)?.label ?? 'System'

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
        <Group head="HOME">
          <div className="set-row">
            <span className="set-row-body">
              <span className="set-label">Arc between sessions</span>
            </span>
            <Status save={save} row="arc" />
            <Switch
              label="Arc between sessions"
              on={loaded.arc}
              disabled={busy}
              onToggle={() => void commitHome('arc', { show_arc_between_sessions: !loaded.arc })}
            />
          </div>
          <FoldRow
            label="Counter"
            value={loaded.counter}
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
        </Group>
      )}

      {loaded && (
        <Group head="DAY">
          <FoldRow
            label="Routines and blocks"
            value={String(loaded.rows.length)}
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
          <div className="set-row">
            <span className="set-row-body">
              <span className="set-label">Nightly plan</span>
              <span className="set-sub">Plans the day and writes the letter</span>
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
          <FoldRow
            label="Nightly letter"
            value={loaded.draft.nightly_time}
            open={open === 'nightly'}
            onToggle={fold('nightly')}
          >
            {open === 'nightly' && (
              <div className="set-fold-body">
                <input
                  type="time"
                  aria-label="Nightly letter"
                  value={loaded.draft.nightly_time}
                  onChange={(e) => edit('nightly_time', e.target.value)}
                  {...commitOn('nightly')}
                />
                <Status save={save} row="nightly" />
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

      <Group head="REACH">
        <PushRow notify={notify} />
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
        <div className="set-row">
          <span className="set-row-body">
            <span className="set-label">Calls</span>
            <span className="set-sub">Needs a number</span>
          </span>
          <Switch label="Calls" on={false} disabled />
        </div>
      </Group>

      <Group head="NOTE">
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
        {/* The prompt editor stays mounted and renders nothing while closed, so an
            unsaved draft survives a detour through another row. */}
        <FoldRow label="How Note talks" open={open === 'persona'} onToggle={fold('persona')}>
          <PersonaSection active={open === 'persona'} notify={notify} />
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
            </div>
          )}
        </FoldRow>
      </Group>

      <Group head="API TOKENS">
        <FoldRow label="Tokens" open={open === 'tokens'} onToggle={fold('tokens')}>
          {open === 'tokens' && <TokensSection notify={notify} />}
        </FoldRow>
      </Group>

      {me.admin && (
        <Group head="ADMIN">
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
        className="set-signout"
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
