import { useState } from 'react'
import { api } from '../../api'
import { useLoad } from '../../hooks'
import { t, type Key } from '../../i18n'
import type { PromptKind } from '../../types'

const PROMPT_KINDS: { id: PromptKind; key: Key }[] = [
  { id: 'talk', key: 'settings.debug.talk' },
  { id: 'trigger', key: 'settings.debug.trigger' },
  { id: 'nightly', key: 'settings.debug.nightly' },
  { id: 'call', key: 'settings.debug.call' },
]

export function DebugSection() {
  const [kind, setKind] = useState<PromptKind>('talk')
  const [asks, setAsks] = useState(0)
  const [built] = useLoad(() => api.debugPrompt(kind), [kind, asks], { blank: true })

  return (
    <div className="set-fold-body">
      <div className="set-acts">
        <select aria-label={t('settings.debug.session')} value={kind} onChange={(e) => setKind(e.target.value as PromptKind)}>
          {PROMPT_KINDS.map((k) => (
            <option key={k.id} value={k.id}>
              {t(k.key)}
            </option>
          ))}
        </select>
        <button className="set-link" onClick={() => setAsks((n) => n + 1)}>
          {t('settings.debug.refresh')}
        </button>
      </div>
      {built === 'error' && <p className="set-sub">{t('settings.debug.failed')}</p>}
      {built && built !== 'error' && (
        <>
          <p className="set-sub">
            {t('settings.debug.size', { chars: built.prompt.length.toLocaleString(), tools: String(built.tools.length) })}
          </p>
          <pre className="mono set-debug">{built.prompt}</pre>
          <p className="set-sub mono">{built.tools.join(' · ')}</p>
        </>
      )}
    </div>
  )
}
