import { useEffect, useState } from 'react'
import { admin, ApiError } from '../../api'
import { useLoad } from '../../hooks'
import { t } from '../../i18n'
import * as format from '../../i18n/format'
import type { TraceCall, TraceRow } from '../../types'
import { Group } from '../../ui/rows'
import { LOG_PAGE, stamp, type Expire } from './common'

const OUTCOMES = ['ok', 'max_turns', 'stopped', 'error'] as const

const unit = (n: number, name: string, digits = 0) =>
  format.number(n, {
    style: 'unit',
    unit: name,
    unitDisplay: 'narrow',
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  })

function duration(ms: number): string {
  if (ms < 1000) return unit(ms, 'millisecond')
  if (ms < 60_000) return unit(ms / 1000, 'second', 1)
  const seconds = Math.round(ms / 1000)
  return t('admin.minutesSeconds', { m: Math.floor(seconds / 60), s: seconds % 60 })
}

function pretty(raw: string): string {
  const text = raw.trim()
  if (!text) return ''
  try {
    return JSON.stringify(JSON.parse(text), null, 2)
  } catch {
    return raw
  }
}

const outcomeClass = (outcome: string) =>
  outcome === 'error' ? 'trace-bad' : outcome === 'max_turns' ? 'trace-warn' : 'set-sub'

export function TracesGroup({ expire }: { expire: Expire }) {
  const [rows, setRows] = useState<TraceRow[]>([])
  const [kinds, setKinds] = useState<string[]>([])
  const [kind, setKind] = useState('')
  const [outcome, setOutcome] = useState('')
  const [state, setState] = useState<'loading' | 'ready' | 'error'>('loading')
  const [open, setOpen] = useState<number | null>(null)

  const fetchPage = (before?: number) => {
    setState('loading')
    if (before === undefined) setOpen(null)
    admin
      .traces({
        limit: LOG_PAGE,
        kind: kind || undefined,
        outcome: outcome || undefined,
        before_id: before,
      })
      .then((page) => {
        setRows((r) => (before === undefined ? page.rows : [...r, ...page.rows]))
        setKinds(page.kinds)
        setState('ready')
      })
      .catch((err) => {
        if (expire(err)) return
        // A server without the route has simply never recorded one.
        if (err instanceof ApiError && err.status === 404) {
          setRows([])
          setState('ready')
          return
        }
        setState('error')
      })
  }

  useEffect(() => {
    fetchPage()
  }, [kind, outcome])

  const last = rows[rows.length - 1]

  return (
    <Group head={t('admin.head.sessions')}>
      <div className="set-fold-body">
        <div className="trace-filters">
          <select aria-label={t('admin.kind')} value={kind} onChange={(e) => setKind(e.target.value)}>
            <option value="">{t('admin.allKinds')}</option>
            {kinds.map((k) => (
              <option key={k} value={k}>
                {k}
              </option>
            ))}
          </select>
          <select
            aria-label={t('admin.outcome')}
            value={outcome}
            onChange={(e) => setOutcome(e.target.value)}
          >
            <option value="">{t('admin.allOutcomes')}</option>
            {OUTCOMES.map((o) => (
              <option key={o} value={o}>
                {o}
              </option>
            ))}
          </select>
        </div>
        {state === 'error' && (
          <p className="set-sub">
            {t('admin.sessionsFailed')}{' '}
            <button className="set-link" onClick={() => fetchPage()}>
              {t('common.retry')}
            </button>
          </p>
        )}
        {rows.length > 0 && (
          <div className="log-scroll">
            <table className="log-table admin-table trace-table">
              <thead>
                <tr>
                  <th>{t('admin.col.time')}</th>
                  <th>{t('admin.col.user')}</th>
                  <th>{t('admin.col.kind')}</th>
                  <th>{t('admin.col.outcome')}</th>
                  <th>{t('admin.col.turns')}</th>
                  <th>{t('admin.col.tools')}</th>
                  <th>{t('admin.col.took')}</th>
                  <th>{t('admin.col.error')}</th>
                </tr>
              </thead>
              <tbody>
                {/* the time button is the row's keyboard handle; its click bubbles to the row */}
                {rows.map((row) => (
                  <tr
                    key={row.id}
                    className={open === row.id ? 'trace-row on' : 'trace-row'}
                    onClick={() => setOpen((o) => (o === row.id ? null : row.id))}
                  >
                    <td className="mono">
                      <button
                        className="trace-open"
                        aria-expanded={open === row.id}
                        title={stamp(row.ts)}
                      >
                        {row.ts.slice(11, 19)}
                      </button>
                    </td>
                    <td>{row.username}</td>
                    <td>{row.kind}</td>
                    <td className={outcomeClass(row.outcome)}>{row.outcome}</td>
                    <td className="mono">{row.turns}</td>
                    <td className="mono">{row.tool_calls}</td>
                    <td className="mono">{duration(row.duration_ms)}</td>
                    <td className="set-sub trace-err" title={row.error ?? undefined}>
                      {row.error}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {state === 'ready' && rows.length === 0 && (
          <p className="set-sub">{t('admin.noSessions')}</p>
        )}
        {open !== null && <TraceFold id={open} expire={expire} />}
        <div className="set-acts">
          <button
            className="set-link"
            disabled={state === 'loading' || !last}
            onClick={() => last && fetchPage(last.id)}
          >
            {t('admin.older')}
          </button>
          <button className="set-link" disabled={state === 'loading'} onClick={() => fetchPage()}>
            {t('admin.refresh')}
          </button>
        </div>
      </div>
    </Group>
  )
}

function TraceFold({ id, expire }: { id: number; expire: Expire }) {
  const [trace] = useLoad(() => admin.trace(id), [id], { blank: true, expire })

  if (trace === undefined) return <p className="set-sub">{t('admin.loading')}</p>
  if (trace === 'error') return <p className="set-sub">{t('admin.sessionFailed')}</p>

  const opening = trace.opening?.trim()
  const reply = trace.reply?.trim()

  return (
    <div className="trace-detail">
      <div className="trace-detail-head">
        <span className="set-label">
          {trace.kind} · {trace.username}
        </span>
        <span className="set-sub">{stamp(trace.ts)}</span>
        <span className={outcomeClass(trace.outcome)}>{trace.outcome}</span>
      </div>
      {trace.error && (
        <p className="pane-status bad" role="alert">
          {trace.error}
        </p>
      )}
      {opening && (
        <div className="trace-text">
          <span className="receipt-tool">{t('admin.opening')}</span>
          <pre className="receipt-block">{opening}</pre>
        </div>
      )}
      {trace.rounds.length === 0 && <p className="set-sub">{t('admin.noRounds')}</p>}
      {trace.rounds.map((round, i) => (
        <div className="trace-round" key={i}>
          <div className="trace-round-head">
            <span className="trace-round-n">{t('admin.round', { n: i + 1 })}</span>
            <span className="set-sub">{duration(round.ms)}</span>
          </div>
          {round.error && <p className="pane-status bad">{round.error}</p>}
          {round.calls?.map((call, j) => <Call key={j} call={call} />)}
        </div>
      ))}
      {reply && (
        <div className="trace-text">
          <span className="receipt-tool">{t('admin.reply')}</span>
          <pre className="receipt-block">{reply}</pre>
        </div>
      )}
      {!trace.full && (
        <p className="set-sub">{t('admin.releaseBuild')}</p>
      )}
    </div>
  )
}

function Call({ call }: { call: TraceCall }) {
  const [open, setOpen] = useState(false)
  const args = pretty(call.args ?? '')
  const result = pretty(call.result ?? '')
  const body = Boolean(args || result)
  const state = [call.is_error ? 'error' : '', open ? 'open' : ''].filter(Boolean).join(' ')

  return (
    <div className={`receipt ${state}`.trim()}>
      <button
        className="receipt-chip"
        aria-expanded={body ? open : undefined}
        disabled={!body}
        onClick={() => setOpen((v) => !v)}
      >
        <svg className="receipt-mark" viewBox="0 0 24 24" aria-hidden="true">
          {call.is_error ? <path d="M6 6l12 12M18 6L6 18" /> : <path d="M5 12.5l4.5 4.5L19 7.5" />}
        </svg>
        <span className="receipt-text">{call.name}</span>
        {call.error_kind && <span className="trace-kind">{call.error_kind}</span>}
        <span className="trace-ms mono">{duration(call.ms)}</span>
        {body && (
          <svg className="receipt-chev" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M9 6l6 6-6 6" />
          </svg>
        )}
      </button>
      {open && body && (
        <div className="receipt-body">
          {args && <pre className="receipt-block">{args}</pre>}
          {result && <pre className="receipt-block">{result}</pre>}
        </div>
      )}
    </div>
  )
}
