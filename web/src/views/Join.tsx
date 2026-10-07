import { useEffect, useState, type FormEvent } from 'react'
import { api, ApiError } from '../api'
import { t } from '../i18n'
import type { JoinInfo } from '../types'
import '../styles/onboarding.css'
import '../styles/share.css'

const MIN_PASSWORD = 8
const USERNAME = /^[A-Za-z0-9_-]{1,64}$/

export function JoinPage({ token }: { token: string }) {
  const [info, setInfo] = useState<JoinInfo | 'closed' | 'error' | undefined>(undefined)
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    api.join
      .info(token)
      .then((i) => {
        setInfo(i)
        if (i.username) setUsername(i.username)
      })
      .catch((e: unknown) => setInfo(e instanceof ApiError && e.status === 404 ? 'closed' : 'error'))
  }, [token])

  const name = username.trim()
  const nameOk = USERNAME.test(name)

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    if (busy) return
    if (!nameOk) return setError(t('join.badName'))
    if (password.length < MIN_PASSWORD) return setError(t('join.short', { min: MIN_PASSWORD }))
    setBusy(true)
    setError(null)
    try {
      await api.join.submit(token, name, password)
      location.replace('/')
    } catch (err) {
      const status = err instanceof ApiError ? err.status : 0
      if (status === 404) setInfo('closed')
      else if (status === 409) setError(t('join.taken'))
      else if (status === 422) setError(t('join.badName'))
      else if (status === 429) setError(t('login.throttled'))
      else setError(t('join.failed'))
      setBusy(false)
    }
  }

  if (info === undefined) return null
  if (info === 'closed' || info === 'error')
    return (
      <main className="share-state">
        <h1>{info === 'closed' ? t('join.closed') : t('share.unreachable')}</h1>
        <p>{info === 'closed' ? t('join.closedHint') : t('share.unreachableHint')}</p>
      </main>
    )

  return (
    <div className="login join">
      <h1>{t('app.name')}</h1>
      <p className="join-lede">{t('join.invited')}</p>
      <form onSubmit={(e) => void submit(e)}>
        <div className="field">
          <input
            placeholder={t('login.username')}
            aria-label={t('login.username')}
            autoComplete="username"
            autoCapitalize="none"
            spellCheck={false}
            maxLength={64}
            value={username}
            onChange={(e) => setUsername(e.target.value)}
          />
        </div>
        <div className="field">
          <input
            type="password"
            placeholder={t('join.password')}
            aria-label={t('join.password')}
            autoComplete="new-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </div>
        {error && <p role="alert">{error}</p>}
        <button className="primary" disabled={busy || !name || !password}>
          {t('join.join')}
        </button>
      </form>
    </div>
  )
}
