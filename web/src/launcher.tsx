import { useEffect, useLayoutEffect, useRef } from 'react'
import { launcherShell } from './desktop'
import { t } from './i18n'
import { Jot } from './jot'
import { connectEvents } from './ws'
import './styles/launcher.css'

// The desktop app's launcher window: one jot, floating over everything, whose window follows its height.
export function Launcher({ glass }: { glass: boolean }) {
  const shell = launcherShell()
  const root = useRef<HTMLDivElement>(null)

  useEffect(() => connectEvents(() => {}, () => {}, () => false), [])

  useLayoutEffect(() => {
    document.documentElement.classList.add('launcher-page')
    document.documentElement.dataset.glass = String(glass)
    const el = root.current
    if (!el || !shell) return
    const fit = () => shell.resize(el.getBoundingClientRect().height)
    const watch = new ResizeObserver(fit)
    watch.observe(el)
    fit()
    return () => watch.disconnect()
  }, [shell, glass])

  useEffect(() => {
    const focus = () => root.current?.querySelector('input')?.focus()
    focus()
    const stop = shell?.onShown(focus)
    const escape = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return
      e.preventDefault()
      e.stopPropagation()
      shell?.hide()
    }
    window.addEventListener('keydown', escape, true)
    return () => {
      stop?.()
      window.removeEventListener('keydown', escape, true)
    }
  }, [shell])

  return (
    <div className="launcher" ref={root}>
      <Jot
        flow
        placeholder={t('jot.tellNote')}
        openTalk={() => shell?.open(null)}
        openConversation={(id) => shell?.open(id)}
      />
    </div>
  )
}
