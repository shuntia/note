import { useState, type FormEvent } from 'react'
import { admin, ApiError } from '../../api'
import { t } from '../../i18n'
import type { AdminGate } from '../../types'
import { signChallenge } from '../../webauthn'

export function Gate({
  gate,
  onElevated,
  onBack,
}: {
  gate: AdminGate
  onElevated: () => void
  onBack: () => void
}) {
  const [password, setPassword] = useState('')
  const [code, setCode] = useState('')
  const [useCode, setUseCode] = useState(!gate.methods.passkey)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const { passkey, totp } = gate.methods
  const nothingEnrolled = gate.require_second_factor && !passkey && !totp
  const needsCode = gate.require_second_factor && useCode && totp
  const unenrolled = t('admin.unenrolled')

  const attempt = async (run: () => Promise<void>) => {
    setBusy(true)
    setError(null)
    try {
      await run()
      setPassword('')
      setCode('')
      onElevated()
    } catch (err) {
      const status = err instanceof ApiError ? err.status : 0
      if (status === 401) setError(t('admin.wrong'))
      else if (status === 429) setError(t('admin.throttled'))
      else if (status === 503) setError(unenrolled)
      else if (err instanceof DOMException) setError(t('admin.passkeyUnconfirmed'))
      else setError(t('admin.unlockFailed'))
    } finally {
      setBusy(false)
    }
  }

  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (gate.require_second_factor && passkey && !useCode) {
      void attempt(async () => {
        const challenge = await admin.elevateChallenge()
        await admin.elevateWithPasskey(password, await signChallenge(challenge))
      })
      return
    }
    void attempt(() => admin.elevate(password, needsCode ? code : undefined))
  }

  return (
    <div className="login admin-gate">
      <h1>{t('admin.admin')}</h1>
      {nothingEnrolled ? (
        <p className="admin-gate-note" role="alert">
          {unenrolled}
        </p>
      ) : (
        <form onSubmit={submit}>
          <div className="field">
            <input
              type="password"
              placeholder={t('admin.password')}
              autoComplete="current-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          </div>
          {needsCode && (
            <div className="field">
              <input
                placeholder={t('admin.code')}
                inputMode="numeric"
                autoComplete="one-time-code"
                pattern="\d{6}"
                maxLength={6}
                value={code}
                onChange={(e) => setCode(e.target.value)}
              />
            </div>
          )}
          {!gate.require_second_factor && gate.inspect && (
            <p className="admin-gate-dev">{t('admin.devPasswordOnly')}</p>
          )}
          {error && <p role="alert">{error}</p>}
          <button
            className="primary"
            disabled={busy || !password || (needsCode && code.length !== 6)}
          >
            {gate.require_second_factor && passkey && !useCode ? t('admin.usePasskey') : t('admin.unlock')}
          </button>
          {gate.require_second_factor && passkey && totp && (
            <button
              type="button"
              className="set-link admin-gate-alt"
              onClick={() => {
                setError(null)
                setUseCode((c) => !c)
              }}
            >
              {useCode ? t('admin.usePasskeyInstead') : t('admin.useCodeInstead')}
            </button>
          )}
        </form>
      )}
      <button className="set-link admin-gate-back" onClick={onBack}>
        ← {t('nav.settings')}
      </button>
    </div>
  )
}
