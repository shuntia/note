import { useState, type FormEvent } from 'react'
import { api, ApiError } from '../../api'
import { useLoad } from '../../hooks'
import { t } from '../../i18n'
import type { Token, TokenCreated } from '../../types'
import { dayOf, type Notify } from './common'

export function TokensSection({ notify }: { notify: Notify }) {
  const [tokens, , setTokens] = useLoad(api.tokens, [])
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const [fresh, setFresh] = useState<TokenCreated | null>(null)
  // Revoke is two taps: the first arms the row, the second removes it.
  const [arming, setArming] = useState<number | null>(null)

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
