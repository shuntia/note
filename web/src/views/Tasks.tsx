import { useEffect, useState, type FormEvent } from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import type { Task } from '../types'

export function Tasks({ notify }: ViewProps) {
  const [tasks, setTasks] = useState<Task[] | null>(null)
  const [title, setTitle] = useState('')
  const [busy, setBusy] = useState(false)
  const [failed, setFailed] = useState(false)

  const load = () => {
    api
      .tasks()
      .then((ts) => {
        setTasks(ts)
        setFailed(false)
      })
      .catch(() => setFailed(true))
  }
  useEffect(load, [])

  const add = async (e: FormEvent) => {
    e.preventDefault()
    if (busy || !title.trim()) return
    setBusy(true)
    try {
      await api.addTask(title.trim())
      setTitle('')
      load()
    } catch (err) {
      notify(err instanceof ApiError ? err.message : "Couldn't add the task. Try again.")
    } finally {
      setBusy(false)
    }
  }

  const setState = async (t: Task, state: Task['state']) => {
    try {
      await api.patchTask(t.id, state)
      load()
    } catch {
      notify("Couldn't update the task. Try again.")
    }
  }

  if (failed)
    return (
      <div className="page">
        <p className="muted">
          Couldn't load tasks.{' '}
          <button className="quiet" onClick={load}>
            Retry
          </button>
        </p>
      </div>
    )
  if (tasks === null) return null

  const open = tasks.filter((t) => t.state !== 'done').length
  return (
    <div className="page">
      <h2 className="pane-title">
        <span className="pane-glyph" aria-hidden="true" />
        Tasks
        <span className="pane-meta mono">{open} open</span>
      </h2>
      <form className="quick-add" onSubmit={add}>
        <input placeholder="Add a task…" value={title} onChange={(e) => setTitle(e.target.value)} />
        <button className="primary" disabled={busy || !title.trim()}>
          Add
        </button>
      </form>
      {tasks.length === 0 ? (
        <p className="muted task-empty">No tasks yet. Add one above.</p>
      ) : (
        <ul className="task-list">
          {tasks.map((t) => (
            <li key={t.id} className={`task-row ${t.state === 'done' ? 'done' : ''}`}>
              <input
                type="checkbox"
                className="check"
                checked={t.state === 'done'}
                onChange={(e) => setState(t, e.target.checked ? 'done' : 'open')}
                aria-label={`mark ${t.title} ${t.state === 'done' ? 'open' : 'done'}`}
              />
              <span className="title">{t.title}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
