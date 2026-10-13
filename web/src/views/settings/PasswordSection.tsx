import { useState, type FormEvent } from 'react'
import { api, ApiError } from '../../api'
import { useSave } from '../../hooks'
import { t } from '../../i18n'
import { Status } from '../../ui/rows'

const MIN_PASSWORD = 8

export function PasswordSection() {
  const [current, setCurrent] = useState('')
  const [next, setNext] = useState('')
  const [confirm, setConfirm] = useState('')
  const { save, busy, run } = useSave()

  const ready = current !== '' && next.length >= MIN_PASSWORD && next === confirm

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    if (!ready) return
    await run(
      async () => {
        await api.changePassword(current, next)
        setCurrent('')
        setNext('')
        setConfirm('')
        return t('settings.password.changed')
      },
      (err) =>
        err instanceof ApiError
          ? err.status === 401
            ? t('settings.password.wrong')
            : err.message
          : t('settings.password.failed'),
    )
  }

  return (
    <form className="set-fold-body" onSubmit={(e) => void submit(e)}>
      <input
        type="password"
        aria-label={t('settings.password.current')}
        placeholder={t('settings.password.current')}
        autoComplete="current-password"
        value={current}
        onChange={(e) => setCurrent(e.target.value)}
      />
      <input
        type="password"
        aria-label={t('settings.password.new')}
        placeholder={t('settings.password.new')}
        autoComplete="new-password"
        value={next}
        onChange={(e) => setNext(e.target.value)}
      />
      <input
        type="password"
        aria-label={t('settings.password.confirm')}
        placeholder={t('settings.password.confirm')}
        autoComplete="new-password"
        value={confirm}
        onChange={(e) => setConfirm(e.target.value)}
      />
      <div className="set-acts">
        <button type="submit" className="btn-haze small" disabled={!ready || busy}>
          {t('settings.password.change')}
        </button>
        <Status save={save} />
      </div>
    </form>
  )
}
