import { useEffect, useState, type FormEvent } from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import type { Task } from '../types'

export function Tasks({ notify }: ViewProps) {
  const [tasks, setTasks] = useState<Task[] | null>(null)
  const [title, setTitle] = useState('')
  const [busy, setBusy] = useState(false)

  const load = () => {
    api
      .tasks()
      .then(setTasks)
      .catch(() => notify("Couldn't load tasks. Try again."))
  }
  useEffect(load, [])

  const add = async (e: FormEvent) => {
    e.preventDefault()
    if (!title.trim()) return
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

  if (tasks === null) return null

  return (
    <div>
      {tasks.length === 0 && <p className="muted">No tasks yet. Add one below.</p>}
      {tasks.map((t) => (
        <div key={t.id} className={`task-row ${t.state === 'done' ? 'done' : ''}`}>
          <input
            type="checkbox"
            checked={t.state === 'done'}
            onChange={(e) => setState(t, e.target.checked ? 'done' : 'open')}
            aria-label={`mark ${t.title} ${t.state === 'done' ? 'open' : 'done'}`}
            style={{ width: 'auto' }}
          />
          <span className="title">{t.title}</span>
        </div>
      ))}
      <form className="quick-add" onSubmit={add}>
        <input placeholder="Add a task…" value={title} onChange={(e) => setTitle(e.target.value)} />
        <button className="primary" disabled={busy || !title.trim()}>
          Add
        </button>
      </form>
    </div>
  )
}
