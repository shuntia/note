import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { disablePush, enablePush, pushState } from '../push'
import { SectionTitle } from '../section'
import { applyTheme, saveTheme, storedTheme, type ThemeChoice } from '../theme'
import type { LogRow, Me, PromptDoc, PromptName, Settings as UserSettings } from '../types'

type SectionId = 'profile' | 'schedule' | 'appearance' | 'persona' | 'notifications' | 'admin'

const SECTIONS: { id: SectionId; label: string }[] = [
  { id: 'profile', label: 'Profile' },
  { id: 'schedule', label: 'Schedule' },
  { id: 'appearance', label: 'Appearance' },
  { id: 'persona', label: 'Persona' },
  { id: 'notifications', label: 'Notifications' },
  { id: 'admin', label: 'Server log' },
]

export function Settings({
  me,
  notify,
  onSignedOut,
}: ViewProps & { me: Me; onSignedOut: () => void }) {
  const [section, setSection] = useState<SectionId>('profile')
  const sections = SECTIONS.filter((s) => s.id !== 'admin' || me.admin)

  return (
    <div className="settings">
      <div className="set-side">
        <nav className="set-nav" aria-label="Settings sections">
          {sections.map((s) => (
            <button
              key={s.id}
              className="set-item"
              aria-current={section === s.id}
              onClick={() => setSection(s.id)}
            >
              {s.label}
            </button>
          ))}
        </nav>
        <div className="set-foot">
          <p className="set-who muted">Signed in as {me.username}.</p>
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
        </div>
      </div>

      {/* Sections that hold unsaved work stay mounted and render nothing while inactive,
          so a draft survives a detour through another section. */}
      <div className="set-pane">
        <ProfileAndSchedule section={section} />
        <PersonaSection active={section === 'persona'} />
        {section === 'appearance' && <AppearanceSection />}
        {section === 'notifications' && <PushSection notify={notify} />}
        {section === 'admin' && me.admin && <AdminSection />}
      </div>
    </div>
  )
}

const EDITABLE = ['display_name', 'timezone', 'nightly_time', 'template'] as const

type Draft = Pick<UserSettings, (typeof EDITABLE)[number]>
type Choices = Pick<UserSettings, 'templates' | 'timezones'>
type Loaded = { choices: Choices; baseline: Draft; draft: Draft }
type SaveState = { kind: 'idle' | 'busy' | 'saved' } | { kind: 'failed'; message: string }

function draftOf(s: UserSettings): Draft {
  return {
    display_name: s.display_name,
    timezone: s.timezone,
    nightly_time: s.nightly_time,
    template: s.template,
  }
}

// Profile and Schedule are one form over one `PUT /api/settings`; the section
// picks which fields are on screen, not which state is live.
function ProfileAndSchedule({ section }: { section: SectionId }) {
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

  const profile = section === 'profile'
  if (section !== 'profile' && section !== 'schedule') return null
  const title = profile ? 'Profile' : 'Schedule'

  if (state === undefined) {
    return (
      <section className="card pane">
        <SectionTitle>{title}</SectionTitle>
        <p className="muted">Loading…</p>
      </section>
    )
  }
  if (state === 'error') {
    return (
      <section className="card pane">
        <SectionTitle>{title}</SectionTitle>
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
      <SectionTitle>{title}</SectionTitle>
      <form onSubmit={submit}>
        <fieldset className="pane-rows" disabled={save.kind === 'busy'}>
          {profile ? (
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
              <p className="pane-hint">What the assistant calls you in briefs and replies.</p>
            </div>
          ) : (
            <>
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
                <p className="pane-hint">Your days start and end here. Type a city to search.</p>
              </div>

              <div className="pane-row">
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
                <p className="pane-hint">When Note writes the morning letter and plans tomorrow.</p>
              </div>

              <div className="pane-row">
                <label className="pane-label" htmlFor="set-template">
                  Shape of the day
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
                <p className="pane-hint">Which routines and blocks make up a day.</p>
              </div>
            </>
          )}
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

const PROMPTS: { id: PromptName; title: string; label: string; hint: string }[] = [
  {
    id: 'persona',
    title: 'Persona',
    label: 'Persona',
    hint: "The assistant's system prompt — its voice and the rules it holds to in every reply.",
  },
  {
    id: 'planning',
    title: 'Planning',
    label: 'Planning prompt',
    hint: 'The system prompt for the nightly session that reshapes tomorrow.',
  },
]

function PersonaSection({ active }: { active: boolean }) {
  const [name, setName] = useState<PromptName>('persona')
  // Fetched body and unsaved text are both kept per prompt, so switching the picker
  // holds on to a draft instead of discarding it.
  const [docs, setDocs] = useState<Partial<Record<PromptName, PromptDoc | 'error'>>>({})
  const [edits, setEdits] = useState<Partial<Record<PromptName, string>>>({})
  const [save, setSave] = useState<SaveState>({ kind: 'idle' })
  const [confirming, setConfirming] = useState(false)
  const [resets, setResets] = useState(0)
  const asked = useRef(new Set<PromptName>())
  const editor = useRef<HTMLTextAreaElement>(null)

  const load = (which: PromptName) => {
    asked.current.add(which)
    setDocs((all) => ({ ...all, [which]: undefined }))
    setSave({ kind: 'idle' })
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

  const prompt = PROMPTS.find((p) => p.id === name) ?? PROMPTS[0]
  const doc = docs[name]
  const picker = (
    <div className="pane-row">
      <label className="pane-label" htmlFor="set-prompt">
        Prompt
      </label>
      <select
        id="set-prompt"
        value={name}
        onChange={(e) => {
          setSave({ kind: 'idle' })
          setName(e.target.value as PromptName)
        }}
      >
        {PROMPTS.map((p) => (
          <option key={p.id} value={p.id}>
            {p.label}
          </option>
        ))}
      </select>
      <p className="pane-hint">{prompt.hint}</p>
    </div>
  )

  if (doc === undefined || doc === 'error') {
    return (
      <section className="card pane">
        <SectionTitle>{prompt.title}</SectionTitle>
        <div className="pane-rows">{picker}</div>
        {doc === undefined ? (
          <p className="muted set-prompt-note">Loading…</p>
        ) : (
          <p className="muted set-prompt-note">
            The prompt didn't load.{' '}
            <button className="quiet" onClick={() => load(name)}>
              Retry
            </button>
          </p>
        )}
      </section>
    )
  }

  const text = edits[name] ?? doc.content
  const blank = text.trim().length === 0
  const dirty = text !== doc.content
  const busy = save.kind === 'busy'
  const setText = (value: string) => setEdits((all) => ({ ...all, [name]: value }))

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    if (!dirty || blank) return
    setSave({ kind: 'busy' })
    try {
      const d = await api.promptPut(name, text)
      setDocs((all) => ({ ...all, [name]: d }))
      setSave({ kind: 'saved' })
    } catch (err) {
      setSave({
        kind: 'failed',
        message:
          err instanceof ApiError && err.status === 400
            ? err.message
            : "The prompt didn't save. Try again.",
      })
    }
  }

  const reset = async () => {
    setConfirming(false)
    setSave({ kind: 'busy' })
    try {
      const d = await api.promptReset(name)
      setDocs((all) => ({ ...all, [name]: d }))
      setEdits((all) => ({ ...all, [name]: undefined }))
      setSave({ kind: 'idle' })
    } catch {
      setSave({ kind: 'failed', message: "The prompt didn't reset. Try again." })
    } finally {
      setResets((n) => n + 1)
    }
  }

  return (
    <section className="card pane">
      <SectionTitle>{prompt.title}</SectionTitle>
      <form onSubmit={submit}>
        <fieldset className="pane-rows" disabled={busy}>
          {picker}
          <div className="pane-row">
            <div className="set-prompt-head">
              <label className="pane-label" htmlFor="set-prompt-text">
                {prompt.label}
              </label>
              {doc.custom && <span className="set-badge">Customized</span>}
            </div>
            <textarea
              id="set-prompt-text"
              ref={editor}
              className="mono set-prompt"
              rows={16}
              value={text}
              onChange={(e) => setText(e.target.value)}
            />
            <p className="pane-hint">
              {doc.custom
                ? 'Your own wording. Reset to default to go back to the one Note ships.'
                : "Note's default wording. Editing it saves a copy for your account only."}
            </p>
          </div>
        </fieldset>

        <div className="pane-foot">
          <button className="primary" disabled={!dirty || blank || busy}>
            {busy ? 'Saving…' : 'Save prompt'}
          </button>
          <button
            type="button"
            className="quiet"
            disabled={(!doc.custom && !dirty) || busy}
            onClick={() => setConfirming(true)}
          >
            Reset to default
          </button>
          {blank && dirty && (
            <span className="pane-status bad" role="alert">
              A prompt can't be empty.
            </span>
          )}
          {save.kind === 'saved' && !dirty && (
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

      {confirming && (
        <Confirm
          title="Reset this prompt?"
          confirmLabel="Reset to default"
          onCancel={() => setConfirming(false)}
          onConfirm={() => void reset()}
        >
          The {prompt.label.toLowerCase()} goes back to the wording Note ships with. Your version
          is discarded.
        </Confirm>
      )}
    </section>
  )
}

function Confirm({
  title,
  children,
  confirmLabel,
  onCancel,
  onConfirm,
}: {
  title: string
  children: ReactNode
  confirmLabel: string
  onCancel: () => void
  onConfirm: () => void
}) {
  const ref = useRef<HTMLDialogElement>(null)
  const id = useId()
  useEffect(() => {
    ref.current?.showModal()
  }, [])

  // Closing before the caller unmounts us is what hands focus back to the button
  // that opened the dialog.
  const dismiss = (done: () => void) => () => {
    ref.current?.close()
    done()
  }

  return (
    <dialog
      className="dialog"
      ref={ref}
      aria-labelledby={`${id}-title`}
      aria-describedby={`${id}-text`}
      onCancel={(e) => {
        e.preventDefault()
        dismiss(onCancel)()
      }}
      // The dialog element itself is only the click target outside the padded body.
      onClick={(e) => {
        if (e.target === ref.current) dismiss(onCancel)()
      }}
    >
      <div className="dialog-body">
        <h2 className="dialog-title" id={`${id}-title`}>
          {title}
        </h2>
        <p className="dialog-text" id={`${id}-text`}>
          {children}
        </p>
        <div className="dialog-foot">
          <button type="button" className="quiet" onClick={dismiss(onCancel)}>
            Cancel
          </button>
          <button type="button" className="quiet danger" onClick={dismiss(onConfirm)}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </dialog>
  )
}

const THEMES: { id: ThemeChoice; label: string }[] = [
  { id: 'system', label: 'System' },
  { id: 'light', label: 'Light' },
  { id: 'dark', label: 'Dark' },
]

function AppearanceSection() {
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
        <div className="pane-row">
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
          <p className="pane-hint">System follows whatever your device is set to right now.</p>
        </div>
      </div>
    </section>
  )
}

function PushSection({ notify }: { notify: (msg: string) => void }) {
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
    <section className="card pane">
      <SectionTitle>Notifications</SectionTitle>
      <div className="pane-rows">
        <div className="pane-row inline">
          <span className="pane-label">
            Push to this device
            <span className="pane-hint">
              {state === 'unsupported'
                ? 'This browser has no push support, so reminders stay in the app.'
                : 'Reminders reach you here even when Note is closed.'}
            </span>
          </span>
          {state !== 'unsupported' && (
            <button className="quiet" disabled={state === 'busy'} onClick={toggle}>
              {state === 'on' ? 'Turn off' : 'Turn on'}
            </button>
          )}
        </div>
      </div>
    </section>
  )
}

function AdminSection() {
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
      <p className="pane-note muted">The last 100 events across every account on this server.</p>
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
