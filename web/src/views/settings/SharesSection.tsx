import { useEffect, useRef, useState } from 'react'
import { api, ApiError } from '../../api'
import { useLoad } from '../../hooks'
import { t, type Key } from '../../i18n'
import * as format from '../../i18n/format'
import { Overflow } from '../../overflow'
import type { Share, ShareMessage, SharePatch, ShareScope, ShareThread, ShareVisit } from '../../types'
import { Switch } from '../../ui/rows'
import { dayOf, type Notify } from './common'

const EXPIRIES = [7, 30, 120] as const

const DEFAULT_SCOPE: ShareScope = {
  today: true, tasks: true, categories: [], goals: true, progress: true,
  details: false, horizon_days: 3, notes: false, messages_per_day: 40,
}

function inDays(days: number): string {
  return new Date(Date.now() + days * 24 * 60 * 60 * 1000).toISOString()
}

function scopeWords(s: ShareScope): string {
  const parts: string[] = []
  if (s.tasks) parts.push(...(s.categories.length > 0 ? s.categories : [t('settings.share.scope.tasks')]))
  if (s.today) parts.push(t('settings.share.scope.plan'))
  if (s.goals) parts.push(t('settings.share.scope.goals'))
  if (s.progress) parts.push(t('settings.share.scope.progress'))
  const words = parts.length > 0 ? format.unitList(parts) : t('settings.share.scope.nothing')
  return words.charAt(0).toUpperCase() + words.slice(1)
}

function localToday(): string {
  const d = new Date()
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
}

function clampedOr(raw: string, current: number, min: number, max: number): number {
  const n = Number(raw)
  if (raw.trim() === '' || !Number.isFinite(n)) return current
  return Math.min(max, Math.max(min, Math.round(n)))
}

const MESSAGE_FROM: Record<ShareMessage['role'], Key> = {
  note: 'settings.share.fromNote',
  user: 'settings.share.fromThem',
  assistant: 'settings.share.fromAssistant',
}

type ShareDraft = { name: string; brief: string; scope: ShareScope; days: number; date: string }

function ShareForm({
  initial,
  categories,
  submit,
  busy,
  label,
  expiryNote,
}: {
  initial: ShareDraft
  categories: string[]
  submit: (d: ShareDraft) => void
  busy: boolean
  label: string
  expiryNote?: string
}) {
  const [d, setD] = useState<ShareDraft>(initial)
  const pills = [...new Set([...categories, ...d.scope.categories])].sort()
  const filtered = d.scope.today || d.scope.tasks || d.scope.goals || d.scope.progress
  const scope = (patch: Partial<ShareScope>) => setD((x) => ({ ...x, scope: { ...x.scope, ...patch } }))
  const toggleCategory = (c: string) =>
    scope({
      categories: d.scope.categories.includes(c)
        ? d.scope.categories.filter((x) => x !== c)
        : [...d.scope.categories, c],
    })
  return (
    <form
      className="set-share-form"
      onSubmit={(e) => {
        e.preventDefault()
        submit(d)
      }}
    >
      <input aria-label={t('settings.share.linkName')} placeholder={t('settings.share.whoFor')} maxLength={64} value={d.name} onChange={(e) => setD({ ...d, name: e.target.value })} />
      {expiryNote && <span className="set-sub">{expiryNote}</span>}
      <div className="set-row set-share-expiry">
        <span className="set-row-body"><span className="set-label">{t('settings.share.endsAfter')}</span></span>
        <div className="seg" role="group" aria-label={t('settings.share.expiry')}>
          {EXPIRIES.map((days) => (
            <button key={days} type="button" aria-pressed={d.days === days && d.date === ''} onClick={() => setD({ ...d, days, date: '' })}>{t('settings.share.days', { count: days })}</button>
          ))}
        </div>
      </div>
      <div className="set-row">
        <span className="set-row-body"><span className="set-label">{t('settings.share.orEndsOn')}</span></span>
        <input type="date" aria-label={t('settings.share.endsOn')} min={localToday()} value={d.date} onChange={(e) => setD({ ...d, date: e.target.value })} />
      </div>
      <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.plan')}</span></span><Switch label={t('settings.share.sharePlan')} on={d.scope.today} onToggle={() => scope({ today: !d.scope.today })} /></div>
      {d.scope.today && (
        <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.daysAhead')}</span></span><input type="number" aria-label={t('settings.share.daysAhead')} min={1} max={14} value={d.scope.horizon_days} onChange={(e) => scope({ horizon_days: clampedOr(e.target.value, d.scope.horizon_days, 1, 14) })} /></div>
      )}
      <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.tasks')}</span></span><Switch label={t('settings.share.shareTasks')} on={d.scope.tasks} onToggle={() => scope({ tasks: !d.scope.tasks })} /></div>
      {d.scope.tasks && (
        <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.details')}</span></span><Switch label={t('settings.share.shareDetails')} on={d.scope.details} onToggle={() => scope({ details: !d.scope.details })} /></div>
      )}
      <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.goals')}</span></span><Switch label={t('settings.share.shareGoals')} on={d.scope.goals} onToggle={() => scope({ goals: !d.scope.goals })} /></div>
      <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.progress')}</span></span><Switch label={t('settings.share.shareProgress')} on={d.scope.progress} onToggle={() => scope({ progress: !d.scope.progress })} /></div>
      {filtered && pills.length > 0 && (
        <div className="set-share-cats">
          <span className="set-sub">{t('settings.share.categoriesSub')}</span>
          <div className="seg wrap" role="group" aria-label={t('settings.share.categories')}>
            {pills.map((c) => (
              <button key={c} type="button" aria-pressed={d.scope.categories.includes(c)} onClick={() => toggleCategory(c)}>{c}</button>
            ))}
          </div>
        </div>
      )}
      <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.notes')}</span></span><Switch label={t('settings.share.allowNotes')} on={d.scope.notes} onToggle={() => scope({ notes: !d.scope.notes })} /></div>
      <div className="set-row"><span className="set-row-body"><span className="set-label">{t('settings.share.messages')}</span></span><input type="number" aria-label={t('settings.share.messages')} min={1} max={100} value={d.scope.messages_per_day} onChange={(e) => scope({ messages_per_day: clampedOr(e.target.value, d.scope.messages_per_day, 1, 100) })} /></div>
      <textarea aria-label={t('settings.share.brief')} placeholder={t('settings.share.briefHint')} rows={3} maxLength={4096} value={d.brief} onChange={(e) => setD({ ...d, brief: e.target.value })} />
      <span className="set-sub">{t('settings.share.briefSub')}</span>
      <button type="submit" className="btn-haze small" disabled={busy || !d.name.trim()}>{label}</button>
    </form>
  )
}

export function SharesSection({ notify }: { notify: Notify }) {
  const [shares, , setShares] = useLoad(api.shares, [])
  const [categories, setCategories] = useState<string[]>([])
  const [creating, setCreating] = useState(false)
  const [editing, setEditing] = useState<number | null>(null)
  const [openThreads, setOpenThreads] = useState<number | null>(null)
  const [threads, setThreads] = useState<ShareThread[] | undefined>(undefined)
  const [visits, setVisits] = useState<ShareVisit[] | undefined>(undefined)
  const [busy, setBusy] = useState(false)
  const [fresh, setFresh] = useState<Share | null>(null)
  const threadsFor = useRef<number | null>(null)

  useEffect(() => {
    api
      .tasks()
      .then((ts) => setCategories([...new Set(ts.map((x) => x.category).filter((c) => c !== ''))].sort()))
      .catch(() => setCategories([]))
  }, [])

  const expiresOf = (d: ShareDraft) => (d.date ? new Date(`${d.date}T23:59:00`).toISOString() : inDays(d.days))

  const create = async (d: ShareDraft) => {
    setBusy(true)
    try {
      const made = await api.createShare({ name: d.name.trim(), brief: d.brief, scope: d.scope, expires_at: expiresOf(d) })
      setShares((all) => (Array.isArray(all) ? [...all, made] : all))
      setFresh(made)
      setCreating(false)
    } catch (err) {
      notify(err instanceof ApiError && (err.status === 409 || err.status === 422) ? err.message : t('settings.share.createFailed'))
    } finally {
      setBusy(false)
    }
  }

  const save = async (s: Share, d: ShareDraft) => {
    setBusy(true)
    try {
      const body: SharePatch = { name: d.name.trim(), brief: d.brief, scope: d.scope }
      if (d.date || d.days !== 0) body.expires_at = expiresOf(d)
      const up = await api.updateShare(s.id, body)
      setShares((all) => (Array.isArray(all) ? all.map((x) => (x.id === s.id ? up : x)) : all))
      setEditing(null)
      notify(t('settings.share.saved'))
    } catch (err) {
      notify(err instanceof ApiError && err.status === 422 ? err.message : t('settings.share.saveFailed'))
    } finally {
      setBusy(false)
    }
  }

  const doRevoke = async (s: Share) => {
    try {
      await api.revokeShare(s.id)
      setShares((all) => (Array.isArray(all) ? all.filter((x) => x.id !== s.id) : all))
      if (fresh?.id === s.id) setFresh(null)
      notify(t('settings.share.revoked'))
    } catch {
      notify(t('settings.share.revokeFailed'))
    }
  }

  const copy = (s: Share) => {
    const fallback = () => notify(t('settings.share.copyFallback'))
    if (navigator.clipboard) navigator.clipboard.writeText(s.url).then(() => notify(t('settings.share.copied')), fallback)
    else fallback()
    setFresh(s)
  }

  const showThreads = (s: Share) => {
    if (openThreads === s.id) {
      threadsFor.current = null
      setOpenThreads(null)
      return
    }
    threadsFor.current = s.id
    setOpenThreads(s.id)
    setThreads(undefined)
    setVisits(undefined)
    const land = (ts: ShareThread[]) => {
      if (threadsFor.current === s.id) setThreads(ts)
    }
    const landVisits = (vs: ShareVisit[]) => {
      if (threadsFor.current === s.id) setVisits(vs)
    }
    api.shareThreads(s.id).then(land).catch(() => land([]))
    api.shareVisits(s.id).then(landVisits).catch(() => landVisits([]))
  }

  const blank: ShareDraft = { name: '', brief: '', scope: DEFAULT_SCOPE, days: 30, date: '' }
  const draftOf = (s: Share): ShareDraft => ({ name: s.name, brief: s.brief, scope: s.scope, days: 0, date: '' })

  return (
    <div className="set-fold-body">
      <span className="set-sub">{t('settings.share.intro')}</span>
      {!creating && <button type="button" className="btn-haze small" onClick={() => setCreating(true)}>{t('settings.share.new')}</button>}
      {creating && <ShareForm initial={blank} categories={categories} submit={(d) => void create(d)} busy={busy} label={t('settings.share.create')} />}
      {fresh && (
        <div className="set-token-fresh">
          <code className="set-token-secret">{fresh.url}</code>
          <span className="set-sub">{t('settings.share.anyone', { name: fresh.name, day: dayOf(fresh.expires_at) })}</span>
        </div>
      )}
      {shares === 'error' && <span className="set-sub">{t('settings.share.loadFailed')}</span>}
      {Array.isArray(shares) && shares.length === 0 && !creating && <span className="set-sub">{t('settings.share.none')}</span>}
      {Array.isArray(shares) &&
        shares.map((s) => (
          <div key={s.id} className="set-share">
            <div className="set-row set-token-row">
              <span className="set-row-body">
                <span className="set-label">
                  {s.name}
                  {s.distant_visits > 0 && <i className="set-share-far" role="img" aria-label={t('settings.share.far')} title={t('settings.share.far')} />}
                </span>
                <span className="set-sub">
                  {t('settings.share.summary', {
                    scope: scopeWords(s.scope),
                    day: dayOf(s.expires_at),
                    visitors: s.visitors,
                    messages: s.messages_today,
                  })}
                </span>
              </span>
              <Overflow
                className="set-share-more"
                label={t('settings.share.moreFor', { name: s.name })}
                items={[
                  { label: t('settings.share.copy'), run: () => copy(s) },
                  { label: t('settings.share.preview'), run: () => window.open(s.url, '_blank', 'noopener') },
                  {
                    label: openThreads === s.id ? t('settings.share.hideActivity') : t('settings.share.activity'),
                    run: () => showThreads(s),
                  },
                  {
                    label: editing === s.id ? t('settings.share.stopEditing') : t('menu.edit'),
                    run: () => setEditing(editing === s.id ? null : s.id),
                  },
                  {
                    label: t('settings.share.revoke'),
                    kind: 'danger',
                    run: () =>
                      notify(t('settings.share.revokeAsk'), {
                        label: t('settings.share.revoke'),
                        run: () => void doRevoke(s),
                      }),
                  },
                ]}
              />
            </div>
            {editing === s.id && (
              <ShareForm
                initial={draftOf(s)}
                categories={categories}
                submit={(d) => void save(s, d)}
                busy={busy}
                label={t('common.save')}
                expiryNote={t('settings.share.currently', { day: dayOf(s.expires_at) })}
              />
            )}
            {openThreads === s.id && (
              <div className="set-share-threads">
                {visits && visits.length > 0 && (
                  <ul className="set-share-visits">
                    {visits.map((v, i) => (
                      <li key={i} className={v.distant ? 'far' : undefined}>
                        {[
                          [v.city, v.country].filter(Boolean).join(', ') || t('settings.share.somewhere'),
                          v.km === null ? null : t('settings.share.km', { n: format.number(Math.round(v.km)) }),
                          dayOf(v.at),
                        ]
                          .filter(Boolean)
                          .join(' · ')}
                      </li>
                    ))}
                  </ul>
                )}
                {threads === undefined && <span className="set-sub">{t('settings.share.loading')}</span>}
                {threads && threads.length === 0 && <span className="set-sub">{t('settings.share.noQuestions')}</span>}
                {threads?.map((th) => (
                  <div key={th.id} className="set-share-thread">
                    <span className="set-sub">
                      {t('settings.share.visitor', { from: dayOf(th.created_at), last: dayOf(th.updated_at) })}
                    </span>
                    {th.messages.map((m, i) => (
                      <p key={i} className={`set-share-msg ${m.role}`}>
                        {t(MESSAGE_FROM[m.role], { text: m.content })}
                      </p>
                    ))}
                  </div>
                ))}
              </div>
            )}
          </div>
        ))}
    </div>
  )
}
