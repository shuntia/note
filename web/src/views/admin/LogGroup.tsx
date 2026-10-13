import { useEffect, useState } from 'react'
import { admin } from '../../api'
import { t } from '../../i18n'
import type { LogRow } from '../../types'
import { Group } from '../../ui/rows'
import { LOG_PAGE, stamp, type Expire } from './common'

export function LogGroup({ expire }: { expire: Expire }) {
  const [rows, setRows] = useState<LogRow[]>([])
  const [kinds, setKinds] = useState<string[]>([])
  const [kind, setKind] = useState('')
  const [state, setState] = useState<'loading' | 'ready' | 'error'>('loading')

  const fetchPage = (which: string, before?: number) => {
    setState('loading')
    admin
      .log({ limit: LOG_PAGE, kind: which || undefined, before_id: before })
      .then((page) => {
        setRows((r) => (before === undefined ? page.rows : [...r, ...page.rows]))
        setKinds(page.kinds)
        setState('ready')
      })
      .catch((err) => {
        if (!expire(err)) setState('error')
      })
  }

  useEffect(() => {
    fetchPage(kind)
  }, [kind])

  const last = rows[rows.length - 1]

  return (
    <Group head={t('admin.head.log')}>
      <div className="set-fold-body">
        <select aria-label={t('admin.kind')} value={kind} onChange={(e) => setKind(e.target.value)}>
          <option value="">{t('admin.all')}</option>
          {kinds.map((k) => (
            <option key={k} value={k}>
              {k}
            </option>
          ))}
        </select>
        {state === 'error' && (
          <p className="set-sub">
            {t('admin.logFailed')}{' '}
            <button className="set-link" onClick={() => fetchPage(kind)}>
              {t('common.retry')}
            </button>
          </p>
        )}
        {rows.length > 0 && (
          <div className="log-scroll">
            <table className="log-table">
              <tbody>
                {rows.map((row) => (
                  <tr key={row.id}>
                    <td className="mono" title={stamp(row.ts)}>
                      {row.ts.slice(11, 19)}
                    </td>
                    <td>{row.kind}</td>
                    <td className="set-sub">{row.detail}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {state === 'ready' && rows.length === 0 && <p className="set-sub">{t('admin.nothingLogged')}</p>}
        <div className="set-acts">
          <button
            className="set-link"
            disabled={state === 'loading' || !last}
            onClick={() => last && fetchPage(kind, last.id)}
          >
            {t('admin.older')}
          </button>
          <button
            className="set-link"
            disabled={state === 'loading'}
            onClick={() => fetchPage(kind)}
          >
            {t('admin.refresh')}
          </button>
        </div>
      </div>
    </Group>
  )
}
