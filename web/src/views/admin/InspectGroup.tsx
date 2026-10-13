import { useEffect, useState } from 'react'
import { admin } from '../../api'
import { useLoad, useSave } from '../../hooks'
import { t } from '../../i18n'
import * as format from '../../i18n/format'
import type { AdminUser, Conversation, SqlResult } from '../../types'
import { FoldRow, Group } from '../../ui/rows'
import { message, Status, type Expire } from './common'

export function InspectGroup({ expire }: { expire: Expire }) {
  const [users, setUsers] = useState<AdminUser[]>([])
  const [id, setId] = useState<number | null>(null)
  const [data] = useLoad(() => admin.inspectUser(id!), [id], { blank: true, skip: id === null, expire })
  const [open, setOpen] = useState<string | null>(null)

  useEffect(() => {
    admin
      .users()
      .then(setUsers)
      .catch((err) => void expire(err))
  }, [])

  useEffect(() => {
    if (id !== null) setOpen(null)
  }, [id])

  const fold = (key: string) => () => setOpen((o) => (o === key ? null : key))
  const loaded = data !== undefined && data !== 'error' ? data : null

  return (
    <Group head={t('admin.head.inspect')}>
      <div className="set-fold-body">
        <select
          aria-label={t('admin.user')}
          value={id === null ? '' : String(id)}
          onChange={(e) => setId(e.target.value ? Number(e.target.value) : null)}
        >
          <option value="">{t('admin.pickUser')}</option>
          {users.map((u) => (
            <option key={u.id} value={u.id}>
              {u.username}
            </option>
          ))}
        </select>
        {data === 'error' && <p className="set-sub">{t('admin.userFailed')}</p>}
      </div>

      {loaded && id !== null && (
        <>
          <FoldRow label={t('admin.config')} open={open === 'config'} onToggle={fold('config')}>
            {open === 'config' && (
              <ConfigFold
                id={id}
                path={loaded.config_path}
                toml={loaded.config_toml}
                expire={expire}
              />
            )}
          </FoldRow>

          <FoldRow
            label={t('admin.tasks')}
            value={format.number(loaded.tasks.length)}
            open={open === 'tasks'}
            onToggle={fold('tasks')}
          >
            {open === 'tasks' && (
              <div className="set-fold-body">
                <div className="log-scroll">
                  <table className="log-table admin-table">
                    <thead>
                      <tr>
                        <th>{t('admin.col.id')}</th>
                        <th>{t('admin.col.title')}</th>
                        <th>{t('admin.col.state')}</th>
                        <th>{t('admin.col.now')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {loaded.tasks.flatMap((task) => [task, ...task.children]).map((task) => (
                        <tr key={task.id}>
                          <td className="mono">{task.id}</td>
                          <td>{task.title}</td>
                          <td className="set-sub">{task.state}</td>
                          <td className="set-sub">{task.is_now ? t('admin.yes') : ''}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>
            )}
          </FoldRow>

          <FoldRow
            label={t('admin.today')}
            value={format.number(loaded.events_today.length)}
            open={open === 'today'}
            onToggle={fold('today')}
          >
            {open === 'today' && (
              <div className="set-fold-body">
                <div className="log-scroll">
                  <table className="log-table admin-table">
                    <thead>
                      <tr>
                        <th>{t('admin.col.time')}</th>
                        <th>{t('admin.col.kind')}</th>
                        <th>{t('admin.col.status')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {loaded.events_today.map((ev) => (
                        <tr key={ev.id}>
                          <td className="mono">{ev.wall_time}</td>
                          <td>{ev.kind}</td>
                          <td className="set-sub">{ev.status}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>
            )}
          </FoldRow>

          <FoldRow
            label={t('admin.conversations')}
            value={format.number(loaded.conversations.length)}
            open={open === 'conversations'}
            onToggle={fold('conversations')}
          >
            {open === 'conversations' && (
              <ConversationsFold id={id} items={loaded.conversations} expire={expire} />
            )}
          </FoldRow>

          <FoldRow
            label={t('admin.memory')}
            value={format.number(loaded.memory.length)}
            open={open === 'memory'}
            onToggle={fold('memory')}
          >
            {open === 'memory' && (
              <MemoryFold id={id} items={loaded.memory} expire={expire} />
            )}
          </FoldRow>
        </>
      )}

      <FoldRow label={t('admin.sql')} open={open === 'sql'} onToggle={fold('sql')}>
        {open === 'sql' && <SqlFold expire={expire} />}
      </FoldRow>
    </Group>
  )
}

function ConfigFold({
  id,
  path,
  toml,
  expire,
}: {
  id: number
  path: string
  toml: string
  expire: Expire
}) {
  const [text, setText] = useState(toml)
  const { save, busy, run } = useSave()

  const submit = () =>
    run(
      async () => {
        await admin.putConfig(id, text)
      },
      (err) => (expire(err) ? null : message(err, t('admin.saveFailed'))),
    )

  return (
    <div className="set-fold-body">
      <span className="set-sub">{path}</span>
      <textarea
        className="mono set-prompt"
        aria-label={t('admin.userConfig')}
        rows={14}
        value={text}
        onChange={(e) => setText(e.target.value)}
      />
      <div className="set-acts">
        <button
          className="btn-haze small"
          disabled={busy}
          onClick={() => void submit()}
        >
          {t('common.save')}
        </button>
        <Status save={save} />
      </div>
    </div>
  )
}

function ConversationsFold({
  id,
  items,
  expire,
}: {
  id: number
  items: Conversation[]
  expire: Expire
}) {
  const [cid, setCid] = useState<number | null>(null)
  const [messages, reload] = useLoad(() => admin.conversationMessages(id, cid!), [cid], {
    blank: true,
    skip: cid === null,
    expire,
  })

  const openOne = (next: number) => (next === cid ? reload() : setCid(next))

  return (
    <div className="set-fold-body">
      <ul className="admin-list">
        {items.map((c) => (
          <li key={c.id}>
            <button
              className="set-link"
              aria-current={cid === c.id}
              onClick={() => openOne(c.id)}
            >
              {c.title || t('admin.conversation', { id: c.id })}
            </button>
          </li>
        ))}
      </ul>
      {messages === 'error' && <p className="set-sub">{t('admin.messagesFailed')}</p>}
      {Array.isArray(messages) && (
        <div className="log-scroll admin-msgs">
          {messages.map((m) => (
            <p key={m.id}>
              <b>{m.role}</b> {m.content}
            </p>
          ))}
        </div>
      )}
    </div>
  )
}

function MemoryFold({
  id,
  items,
  expire,
}: {
  id: number
  items: { id: string; category: string; summary: string; archived: boolean }[]
  expire: Expire
}) {
  const [mid, setMid] = useState<string | null>(null)
  const [text, setText] = useState('')
  const { save, busy, run } = useSave()

  const openOne = (next: string) => {
    setMid(next)
    setText('')
    void run(
      async () => {
        setText((await admin.memoryGet(id, next)).content)
        return null
      },
      (err) => (expire(err) ? null : t('admin.factFailed')),
    )
  }

  const submit = async () => {
    if (!mid) return
    await run(
      async () => {
        await admin.memoryPut(id, mid, text)
      },
      (err) => (expire(err) ? null : message(err, t('admin.saveFailed'))),
    )
  }

  return (
    <div className="set-fold-body">
      <ul className="admin-list">
        {items.map((m) => (
          <li key={m.id}>
            <button className="set-link" aria-current={mid === m.id} onClick={() => openOne(m.id)}>
              {m.id} · {m.category} · {m.summary}
              {m.archived && <> · {t('admin.archived')}</>}
            </button>
          </li>
        ))}
      </ul>
      {mid && (
        <>
          <textarea
            className="mono set-prompt"
            aria-label={t('admin.memoryFor', { id: mid })}
            rows={10}
            value={text}
            onChange={(e) => setText(e.target.value)}
          />
          <div className="set-acts">
            <button
              className="btn-haze small"
              disabled={busy}
              onClick={() => void submit()}
            >
              {t('common.save')}
            </button>
            <Status save={save} />
          </div>
        </>
      )}
    </div>
  )
}

const cellText = (cell: unknown) => (typeof cell === 'string' ? cell : JSON.stringify(cell))

function SqlFold({ expire }: { expire: Expire }) {
  const [sql, setSql] = useState('')
  const [result, setResult] = useState<SqlResult | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const run = async () => {
    setBusy(true)
    setError(null)
    try {
      setResult(await admin.sql(sql))
    } catch (err) {
      if (expire(err)) return
      setResult(null)
      setError(message(err, t('admin.runFailed')))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="set-fold-body">
      <textarea
        className="mono set-prompt"
        aria-label={t('admin.sql')}
        rows={5}
        spellCheck={false}
        value={sql}
        onChange={(e) => setSql(e.target.value)}
      />
      <div className="set-acts">
        <button className="btn-haze small" disabled={busy || !sql.trim()} onClick={() => void run()}>
          {t('admin.run')}
        </button>
      </div>
      {error && (
        <p className="pane-status bad" role="alert">
          {error}
        </p>
      )}
      {result && 'changes' in result && (
        <p className="set-sub">{t('admin.rowsChanged', { count: result.changes })}</p>
      )}
      {result && 'columns' in result && (
        <>
          <div className="log-scroll">
            <table className="log-table admin-table">
              <thead>
                <tr>
                  {result.columns.map((c) => (
                    <th key={c}>{c}</th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {result.rows.map((row, i) => (
                  <tr key={i}>
                    {row.map((cell, j) =>
                      cell === null ? (
                        <td key={j} className="set-sub">
                          ∅
                        </td>
                      ) : (
                        <td key={j}>{cellText(cell)}</td>
                      ),
                    )}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {result.truncated && <p className="set-sub">{t('admin.truncated', { n: format.number(500) })}</p>}
        </>
      )}
    </div>
  )
}
