import { useState, type FormEvent, type ReactNode } from 'react'
import { api, ApiError } from '../../api'
import { t, type Key } from '../../i18n'
import type { KeyKind, LlmKey } from '../../types'
import type { Notify } from './common'

const KINDS: { id: KeyKind; key: Key }[] = [
  { id: 'openrouter', key: 'key.openrouter' },
  { id: 'openai', key: 'key.openai' },
]

const MODEL_HINT: Record<KeyKind, string> = {
  openrouter: 'openai/gpt-5-mini',
  openai: 'gpt-5-mini',
}

export const keyHint = (k: LlmKey) => `•••• ${k.key_hint}`

function refusal(err: unknown): string {
  if (!(err instanceof ApiError)) return t('key.saveFailed')
  if (err.status === 422) return err.message.startsWith('model') ? t('key.modelRefused') : t('key.refused')
  if (err.status === 502) return t('key.unreachable')
  return t('key.saveFailed')
}

/** Kind, key and model, saved once the provider takes the key. `submit`
 * renders the form's submit control. */
export function KeyForm({
  notify,
  onSaved,
  submit,
  initial,
}: {
  notify: Notify
  onSaved: (k: LlmKey) => void
  submit: (disabled: boolean) => ReactNode
  initial?: LlmKey | null
}) {
  const [kind, setKind] = useState<KeyKind>(initial?.kind ?? 'openrouter')
  const [key, setKey] = useState('')
  const [model, setModel] = useState(initial?.model ?? '')
  const [busy, setBusy] = useState(false)

  const ready = !busy && key.trim() !== '' && model.trim() !== ''

  const save = async (e: FormEvent) => {
    e.preventDefault()
    if (!ready) return
    setBusy(true)
    try {
      const saved = await api.saveLlmKey(kind, key.trim(), model.trim())
      setKey('')
      onSaved(saved)
    } catch (err) {
      notify(refusal(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <form className="set-key-form" onSubmit={(e) => void save(e)}>
      <div className="seg" role="group" aria-label={t('settings.aiKey')}>
        {KINDS.map((k) => (
          <button key={k.id} type="button" aria-pressed={kind === k.id} onClick={() => setKind(k.id)}>
            {t(k.key)}
          </button>
        ))}
      </div>
      <input
        type="password"
        aria-label={t('key.key')}
        placeholder={t('key.key')}
        autoComplete="off"
        spellCheck={false}
        maxLength={512}
        value={key}
        onChange={(e) => setKey(e.target.value)}
      />
      <div className="set-key-row">
        <input
          aria-label={t('key.model')}
          placeholder={MODEL_HINT[kind]}
          autoComplete="off"
          spellCheck={false}
          maxLength={200}
          value={model}
          onChange={(e) => setModel(e.target.value)}
        />
        {submit(!ready)}
      </div>
    </form>
  )
}

export function KeySection({
  notify,
  current,
  onChange,
}: {
  notify: Notify
  current: LlmKey | null
  onChange: (k: LlmKey | null) => void
}) {
  const [arming, setArming] = useState(false)

  const remove = async () => {
    if (!arming) {
      setArming(true)
      return
    }
    setArming(false)
    try {
      await api.removeLlmKey()
      onChange(null)
    } catch {
      notify(t('key.removeFailed'))
    }
  }

  return (
    <div className="set-fold-body">
      <KeyForm
        notify={notify}
        initial={current}
        onSaved={onChange}
        submit={(disabled) => (
          <button type="submit" className="btn-haze small" disabled={disabled}>
            {t('key.save')}
          </button>
        )}
      />
      {current && (
        <div className="set-row set-token-row">
          <span className="set-row-body">
            <span className="set-label">{current.model}</span>
            <span className="set-sub">{keyHint(current)}</span>
          </span>
          <button type="button" className="btn-haze small" onClick={() => void remove()} onBlur={() => setArming(false)}>
            {arming ? t('key.reallyRemove') : t('key.remove')}
          </button>
        </div>
      )}
    </div>
  )
}
