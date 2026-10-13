import { useRef, useState, type FormEvent } from 'react'
import { api, ApiError } from '../../api'
import { useLoad, useSave } from '../../hooks'
import { t } from '../../i18n'
import { Status } from '../../ui/rows'

export function AboutSection({ active }: { active: boolean }) {
  const asked = useRef(false)
  const [saved, load, setSaved] = useLoad(
    () => {
      asked.current = true
      return api.aboutGet().then(
        (d) => d.content,
        (err: unknown) => {
          asked.current = false
          throw err
        },
      )
    },
    [active],
    { blank: true, skip: !active || asked.current },
  )
  const [text, setText] = useState<string | undefined>(undefined)
  const { save, busy, run } = useSave()

  if (!active) return null

  if (saved === undefined || saved === 'error') {
    return (
      <div className="set-fold-body">
        {saved === 'error' && (
          <p className="set-sub">
            {t('settings.about.loadFailed')}{' '}
            <button className="set-link" onClick={load}>
              {t('common.retry')}
            </button>
          </p>
        )}
      </div>
    )
  }

  const value = text ?? saved
  const dirty = value !== saved

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    if (!dirty) return
    await run(
      async () => {
        const d = await api.aboutPut(value)
        setSaved(d.content)
        setText(undefined)
      },
      (err) => (err instanceof ApiError && err.status === 400 ? err.message : t('settings.about.saveFailed')),
    )
  }

  return (
    <form className="set-fold-body" onSubmit={submit}>
      <fieldset className="set-prompt-rows" disabled={busy}>
        <textarea
          className="set-prompt"
          aria-label={t('settings.about')}
          placeholder={t('settings.about.hint')}
          rows={8}
          value={value}
          onChange={(e) => setText(e.target.value)}
        />
      </fieldset>
      <div className="set-acts">
        <button className="btn-haze small" disabled={!dirty || busy}>
          {busy ? t('settings.about.saving') : t('common.save')}
        </button>
        {!dirty && <Status save={save} />}
      </div>
    </form>
  )
}
