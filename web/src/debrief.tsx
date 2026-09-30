import { useEffect, useState } from 'react'
import { api, ApiError } from './api'
import type { Debrief } from './types'
import { t } from './i18n'

const FOLD_KEY = 'note.debriefFolded'

function readFold(date: string): boolean {
  try {
    const raw = localStorage.getItem(FOLD_KEY)
    if (!raw) return true
    const saved = JSON.parse(raw) as { date?: string; folded?: boolean }
    return saved.date === date ? saved.folded !== false : true
  } catch {
    return true
  }
}

function writeFold(date: string, folded: boolean) {
  try {
    localStorage.setItem(FOLD_KEY, JSON.stringify({ date, folded }))
  } catch {
    // storage blocked; the fold still holds for this session
  }
}

// The trailing full stop gives way to the ellipsis rather than stacking with it.
function firstSentence(text: string): string {
  const trimmed = text.trim()
  const match = /^[\s\S]*?[.!?](?=\s|$)/.exec(trimmed)
  const lead = match ? match[0] : trimmed
  if (lead.length === trimmed.length) return lead
  return `${lead.replace(/\.$/, '')}…`
}

export function DebriefFold() {
  const [debrief, setDebrief] = useState<Debrief | null | 'error' | undefined>(undefined)
  const [folded, setFolded] = useState(true)

  const load = () => {
    setDebrief(undefined)
    api
      .debrief()
      .then((d) => {
        setDebrief(d)
        setFolded(readFold(d.date))
      })
      .catch((err) => setDebrief(err instanceof ApiError && err.status === 404 ? null : 'error'))
  }
  useEffect(load, [])

  if (debrief === undefined) return null
  if (debrief === null) {
    return <p className="debrief-note muted">{t('letter.none')}</p>
  }
  if (debrief === 'error') {
    return (
      <p className="debrief-note muted">
        {t('letter.failed')}{' '}
        <button className="quiet" onClick={load}>
          {t('common.retry')}
        </button>
      </p>
    )
  }

  const toggle = () => {
    const next = !folded
    setFolded(next)
    writeFold(debrief.date, next)
  }

  return (
    <section className={`debrief-row${folded ? '' : ' open'}`}>
      <button className="debrief-fold" aria-expanded={!folded} onClick={toggle}>
        <span className="debrief-mark" aria-hidden="true" />
        <span className="debrief-lead">
          <b>{t('letter.lead')}</b> {firstSentence(debrief.content)}
        </span>
        <span className="debrief-chev" aria-hidden="true">
          <svg viewBox="0 0 24 24">
            <path d="M6 9l6 6 6-6" />
          </svg>
        </span>
      </button>
      {!folded && <div className="letter">{debrief.content}</div>}
    </section>
  )
}
