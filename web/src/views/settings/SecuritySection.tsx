import QRCode from 'qrcode'
import { useEffect, useRef, useState, type FormEvent } from 'react'
import { ApiError, security, type FactorProof } from '../../api'
import { useLoad } from '../../hooks'
import { t, type Key } from '../../i18n'
import type { Passkey, SecurityState, TotpEnrolment } from '../../types'
import { createCredential, signChallenge, webauthnSupported, type RegistrationJSON } from '../../webauthn'
import { dayOf, type Notify } from './common'

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
  const [state, load] = useLoad(
    () =>
      security.state().then((s) => {
        onState?.(s)
        return s
      }),
    [],
  )
  const [ask, setAsk] = useState<Ask | null>(null)
  const [password, setPassword] = useState('')
  const [code, setCode] = useState('')
  const [useCode, setUseCode] = useState(false)
  const [naming, setNaming] = useState<{ credential: RegistrationJSON; name: string } | null>(null)
  const [enrol, setEnrol] = useState<TotpEnrolment | null>(null)
  const [rename, setRename] = useState<{ id: number; name: string } | null>(null)
  // Remove is two taps, like revoking a token; the second opens the password prompt.
  const [arming, setArming] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const loaded = state !== undefined && state !== 'error' ? state : null
  const canPasskey = !!loaded?.webauthn_available && webauthnSupported()
  // Once a factor is held, changing any of them is proved with one already held.
  const holdsPasskey = (loaded?.passkeys.length ?? 0) > 0
  const holdsTotp = !!loaded?.totp.enabled
  const proveWithPasskey = holdsPasskey && canPasskey && !(useCode && holdsTotp)
  const proveWithCode = holdsTotp && !proveWithPasskey

  const fail = (err: unknown, fallback: string) =>
    notify(err instanceof ApiError && err.status < 500 && err.message ? err.message : fallback)

  const open = (kind: Ask) => {
    setAsk(kind)
    setPassword('')
    setCode('')
    setArming(null)
  }

  const proof = async (): Promise<FactorProof> => {
    if (proveWithPasskey) return { assertion: await signChallenge(await security.challenge()) }
    return proveWithCode ? { code } : {}
  }

  const answer = async (e: FormEvent) => {
    e.preventDefault()
    if (!ask || busy) return
    setBusy(true)
    try {
      if (ask.kind === 'add-passkey') {
        const challenge = await security.passkeyChallenge(password, await proof())
        setAsk(null)
        setNaming({ credential: await createCredential(challenge), name: '' })
      } else if (ask.kind === 'drop-passkey') {
        await security.removePasskey(ask.id, password, await proof())
        setAsk(null)
        load()
      } else if (ask.kind === 'start-totp') {
        setEnrol(await security.totpStart(password, await proof()))
        setAsk(null)
      } else {
        await security.totpRemove(password, await proof())
        setAsk(null)
        load()
      }
      setPassword('')
      setCode('')
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
          {proveWithCode && (
            <input
              aria-label={t('settings.security.code')}
              placeholder={t('settings.security.code')}
              inputMode="numeric"
              autoComplete="one-time-code"
              pattern="\d{6}"
              maxLength={6}
              value={code}
              onChange={(e) => setCode(e.target.value)}
            />
          )}
          <button
            type="submit"
            className="btn-haze small"
            disabled={busy || !password || (proveWithCode && code.length !== 6)}
          >
            {t(ASK_LABEL[ask.kind].action)}
          </button>
          {holdsPasskey && canPasskey && holdsTotp && (
            <button type="button" className="set-link" onClick={() => setUseCode((c) => !c)}>
              {useCode ? t('settings.security.usePasskeyInstead') : t('settings.security.useCodeInstead')}
            </button>
          )}
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
