import { useEffect, useRef, useState, type FormEvent } from 'react'
import { admin, api } from '../api'
import type { ToastAction } from '../app'
import { adoptLanguage, setLocale, t } from '../i18n'
import { reducedMotion } from '../motion'
import type { Me, SecurityState, Settings } from '../types'
import { deviceZone } from '../zone'
import { SecuritySection } from './Settings'
import '../styles/settings.css'
import '../styles/onboarding.css'

type Step = 'lang' | 'factor' | 'name' | 'zone' | 'done'

const WELCOME_MS = 1800

const city = (tz: string) => tz.split('/').pop()?.replace(/_/g, ' ') ?? tz

function Next({ disabled }: { disabled?: boolean }) {
  return (
    <button type="submit" className="ob-next" aria-label={t('onboard.next')} disabled={disabled}>
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M5 12h14" />
        <path d="M13 6l6 6-6 6" />
      </svg>
    </button>
  )
}

/** The first run after joining by invite: language, a second factor where the
 * admin panel will ask for one, a name, a time zone; then the app. */
export function Onboarding({
  me,
  notify,
  onDone,
}: {
  me: Me
  notify: (msg: string, action?: ToastAction) => void
  onDone: () => void
}) {
  const [settings, setSettings] = useState<Settings | null>(null)
  const [needsFactor, setNeedsFactor] = useState(false)
  const [enrolled, setEnrolled] = useState(false)
  const [step, setStep] = useState<Step>('lang')
  const [name, setName] = useState(me.username)
  const [zone, setZone] = useState(deviceZone())
  const [busy, setBusy] = useState(false)
  const [ready, setReady] = useState(false)
  const finished = useRef(false)

  useEffect(() => {
    const loading = api.settings().then((s) => {
      setSettings(s)
      if (s.display_name && s.display_name !== 'New User') setName(s.display_name)
      if (!s.timezones.includes(deviceZone())) setZone(s.timezone)
    })
    const gate = me.admin
      ? admin.gate().then((g) => setNeedsFactor(g.require_second_factor && g.second_factor === 'none'))
      : null
    void Promise.allSettled([loading, gate]).then(() => setReady(true))
  }, [me.admin])

  const steps: Step[] = ['lang', ...(needsFactor ? (['factor'] as const) : []), 'name', 'zone']

  const save = async (patch: Parameters<typeof api.saveSettings>[0], next: Step) => {
    setBusy(true)
    try {
      await api.saveSettings(patch)
      setStep(next)
    } catch {
      notify(t('onboard.saveFailed'))
    } finally {
      setBusy(false)
    }
  }

  const after = (s: Step): Step => steps[steps.indexOf(s) + 1] ?? 'done'

  const chooseLanguage = (lang: 'en' | 'ja') => {
    adoptLanguage(lang)
    setLocale(lang)
    void save({ language: lang }, after('lang'))
  }

  const submitName = (e: FormEvent) => {
    e.preventDefault()
    if (name.trim()) void save({ display_name: name.trim() }, 'zone')
  }

  const submitZone = (e: FormEvent) => {
    e.preventDefault()
    void save({ timezone: zone, timezone_auto: zone === deviceZone() }, 'done')
  }

  useEffect(() => {
    if (step !== 'done' || finished.current) return
    finished.current = true
    const wait = reducedMotion() ? 300 : WELCOME_MS
    const timer = window.setTimeout(() => {
      api.onboardingDone().then(onDone, onDone)
    }, wait)
    return () => window.clearTimeout(timer)
  }, [step, onDone])

  const onSecurity = (s: SecurityState) => setEnrolled(s.passkeys.length > 0 || s.totp.enabled)

  if (!ready) return null

  return (
    <div className="ob">
      <div className="ob-stage" key={step}>
        {step === 'lang' && (
          <div className="ob-langs">
            <button type="button" lang="ja" className="ob-lang" disabled={busy} onClick={() => chooseLanguage('ja')}>
              {t('settings.language.ja')}
            </button>
            <button type="button" lang="en" className="ob-lang" disabled={busy} onClick={() => chooseLanguage('en')}>
              {t('settings.language.en')}
            </button>
          </div>
        )}

        {step === 'factor' && (
          <div className="ob-factor">
            <h1 className="ob-ask">{t('onboard.factor')}</h1>
            <SecuritySection notify={notify} onState={onSecurity} />
            <div className="ob-row">
              <button type="button" className="set-link" onClick={() => setStep(after('factor'))}>
                {enrolled ? t('onboard.next') : t('onboard.later')}
              </button>
            </div>
          </div>
        )}

        {step === 'name' && (
          <form className="ob-form" onSubmit={submitName}>
            <label className="ob-ask" htmlFor="ob-name">
              {t('onboard.name')}
            </label>
            <div className="ob-row">
              <input
                id="ob-name"
                className="ob-input"
                autoFocus
                maxLength={64}
                autoComplete="nickname"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
              <Next disabled={busy || !name.trim()} />
            </div>
          </form>
        )}

        {step === 'zone' && (
          <form className="ob-form" onSubmit={submitZone}>
            <span className="ob-ask">{t('settings.timezone')}</span>
            <div className="ob-row">
              <label className="ob-zone">
                <span aria-hidden="true">{city(zone)}</span>
                <select
                  aria-label={t('onboard.changeZone')}
                  value={zone}
                  onChange={(e) => setZone(e.target.value)}
                >
                  {(settings?.timezones ?? [zone]).map((z) => (
                    <option key={z} value={z}>
                      {z.replace(/_/g, ' ')}
                    </option>
                  ))}
                </select>
              </label>
              <Next disabled={busy} />
            </div>
          </form>
        )}

        {step === 'done' && (
          <div className="ob-done" role="status">
            <svg className="ob-ring" viewBox="0 0 48 48" aria-hidden="true">
              <circle cx="24" cy="24" r="20" />
            </svg>
            <p className="ob-welcome">{t('onboard.welcome', { name: name.trim() || me.username })}</p>
          </div>
        )}
      </div>

      {step !== 'done' && (
        <ol className="ob-dots" aria-hidden="true">
          {steps.map((s) => (
            <li key={s} className={s === step ? 'on' : steps.indexOf(s) < steps.indexOf(step) ? 'past' : ''} />
          ))}
        </ol>
      )}
    </div>
  )
}
