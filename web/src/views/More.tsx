import { useEffect, useState, type FormEvent } from 'react'
import { api, ApiError } from '../api'
import type { ViewProps } from '../app'
import { disablePush, enablePush, pushState } from '../push'
import type { Debrief, LogRow, Me } from '../types'

export function More({
  me,
  notify,
  onSignedOut,
}: ViewProps & { me: Me; onSignedOut: () => void }) {
  return (
    <div>
      <DebriefCard />
      <PushCard notify={notify} />
      {me.admin && <AdminCard notify={notify} />}
      <h2 className="section-title">Session</h2>
      <button
        className="quiet"
        onClick={async () => {
          try {
            await api.logout()
          } finally {
            onSignedOut()
          }
        }}
      >
        Sign out
      </button>
    </div>
  )
}

function DebriefCard() {
  const [debrief, setDebrief] = useState<Debrief | null | undefined>(undefined)

  useEffect(() => {
    api
      .debrief()
      .then(setDebrief)
      .catch(() => setDebrief(null))
  }, [])

  if (debrief === undefined) return null
  return (
    <div className="card">
      {debrief === null ? (
        <p className="muted">No debrief yet — it arrives overnight.</p>
      ) : (
        <div className="letter">
          <h2>Good morning</h2>
          <div className="muted mono">{debrief.date}</div>
          <p>{debrief.content}</p>
        </div>
      )}
    </div>
  )
}

function PushCard({ notify }: { notify: (msg: string) => void }) {
  const [state, setState] = useState<'unsupported' | 'off' | 'on' | 'busy'>('busy')

  useEffect(() => {
    pushState().then(setState)
  }, [])

  const toggle = async () => {
    const was = state
    setState('busy')
    try {
      if (was === 'on') {
        await disablePush()
        setState('off')
      } else {
        await enablePush()
        setState('on')
      }
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) {
        notify('Web Push is not configured on this server.')
      } else {
        notify("Couldn't change notifications. Try again.")
      }
      setState(was)
    }
  }

  if (state === 'unsupported') return null
  return (
    <>
      <h2 className="section-title">Notifications</h2>
      <button className="quiet" disabled={state === 'busy'} onClick={toggle}>
        {state === 'on' ? 'Turn off push notifications' : 'Turn on push notifications'}
      </button>
    </>
  )
}

function AdminCard({ notify }: { notify: (msg: string) => void }) {
  const [log, setLog] = useState<LogRow[]>([])
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')

  const loadLog = () => {
    api
      .adminLog()
      .then(setLog)
      .catch(() => notify("Couldn't load the log. Try again."))
  }
  useEffect(loadLog, [])

  const create = async (e: FormEvent) => {
    e.preventDefault()
    try {
      await api.adminCreateUser(username, password, false)
      notify(`Created ${username}.`)
      setUsername('')
      setPassword('')
    } catch {
      notify("Couldn't create the user. Check the name and try again.")
    }
  }

  return (
    <>
      <h2 className="section-title">Add a member</h2>
      <form onSubmit={create}>
        <div className="field">
          <input
            placeholder="Username"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
          />
        </div>
        <div className="field">
          <input
            type="password"
            placeholder="Password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </div>
        <button className="primary" disabled={!username || !password}>
          Create
        </button>
      </form>
      <h2 className="section-title">Server log</h2>
      <table className="log-table">
        <tbody>
          {log.map((row, i) => (
            <tr key={i}>
              <td className="mono">{row.ts.slice(11, 19)}</td>
              <td>{row.kind}</td>
              <td className="muted">{row.detail}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </>
  )
}
