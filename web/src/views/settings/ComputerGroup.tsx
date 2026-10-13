import { useEffect, useState } from 'react'
import { ApiError } from '../../api'
import { callsOn, desktop, noticesOn, setCallsOn, setNoticesOn, type ShellSettings } from '../../desktop'
import { t } from '../../i18n'
import { disablePush, enablePush, pushState } from '../../push'
import { Chevron, Group, ToggleRow } from '../../ui/rows'
import type { Notify } from './common'

// The desktop app's own switches: what this computer does, kept by the shell or this window.
export function ComputerGroup() {
  const shell = desktop()!
  const [notices, setNotices] = useState(noticesOn)
  const [calls, setCalls] = useState(callsOn)
  const [settings, setSettings] = useState<ShellSettings | null>(null)

  useEffect(() => {
    shell.settings?.().then(setSettings).catch(() => {})
  }, [shell])

  const flip = (key: keyof ShellSettings, on: boolean) => {
    setSettings((s) => (s ? { ...s, [key]: on } : s))
    shell.set?.(key, on).then((s) => s && setSettings(s)).catch(() => {})
  }

  return (
    <Group head={t('settings.computer')}>
      <ToggleRow
        label={t('settings.desktopNotices')}
        on={notices}
        onToggle={() => {
          setNotices(!notices)
          void setNoticesOn(!notices)
        }}
      />
      <ToggleRow
        label={t('settings.computer.calls')}
        on={calls}
        onToggle={() => {
          setCalls(!calls)
          setCallsOn(!calls)
        }}
      />
      {settings && (
        <ToggleRow
          label={t('settings.computer.startAtLogin')}
          on={settings.startAtLogin}
          onToggle={() => flip('startAtLogin', !settings.startAtLogin)}
        />
      )}
      {settings && settings.nightly !== null && (
        <ToggleRow label={t('settings.computer.nightly')} on={settings.nightly} onToggle={() => flip('nightly', !settings.nightly)} />
      )}
      {shell.changeServer && (
        <button className="set-row set-open" onClick={() => shell.changeServer?.()}>
          <span className="set-row-body">
            <span className="set-label">{t('settings.computer.server')}</span>
            <span className="set-sub">{location.host}</span>
          </span>
          <Chevron />
        </button>
      )}
    </Group>
  )
}

export function PushRow({ notify }: { notify: Notify }) {
  if (desktop()) return null
  return <BrowserPushRow notify={notify} />
}

function BrowserPushRow({ notify }: { notify: Notify }) {
  const [state, setState] = useState<'unsupported' | 'off' | 'on' | 'busy'>('busy')

  useEffect(() => {
    pushState()
      .then(setState)
      .catch(() => setState('unsupported'))
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
        notify(t('settings.push.unconfigured'))
      } else {
        notify(t('settings.push.failed'))
      }
      setState(was)
    }
  }

  return (
    <ToggleRow
      label={t('settings.push')}
      sub={state === 'unsupported' ? t('settings.push.unsupported') : undefined}
      on={state === 'on'}
      disabled={state === 'busy' || state === 'unsupported'}
      onToggle={() => void toggle()}
    />
  )
}
