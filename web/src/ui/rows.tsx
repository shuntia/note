import type { ReactNode } from 'react'
import { t } from '../i18n'
import type { Save } from '../hooks'

export function Group({ head, children }: { head: string; children: ReactNode }) {
  return (
    <section className="set-group">
      <h2 className="set-group-head">{head}</h2>
      {children}
    </section>
  )
}

/** Shows `save` once it settles; with `row`, only when the save is that row's. */
export function Status({ save, row, saved }: { save: Save; row?: string; saved?: string }) {
  if (!save || (row !== undefined && save.row !== row) || save.kind === 'busy') return null
  if (save.kind === 'saved')
    return (
      <span className="pane-status ok" role="status">
        {save.message ?? saved ?? t('settings.saved')}
      </span>
    )
  return (
    <span className="pane-status bad" role="alert">
      {save.message}
    </span>
  )
}

export function Switch({
  label,
  on,
  disabled,
  onToggle,
}: {
  label: string
  on: boolean
  disabled?: boolean
  onToggle?: () => void
}) {
  return (
    <button
      type="button"
      role="switch"
      className="sw"
      aria-checked={on}
      aria-label={label}
      disabled={disabled}
      onClick={onToggle}
    />
  )
}

/** A labelled switch row; `aria` names the switch when the label alone would not, `children` sit before it. */
export function ToggleRow({
  label,
  sub,
  aria,
  className,
  on,
  disabled,
  onToggle,
  children,
}: {
  label: string
  sub?: string
  aria?: string
  className?: string
  on: boolean
  disabled?: boolean
  onToggle: () => void
  children?: ReactNode
}) {
  return (
    <div className={className ? `set-row ${className}` : 'set-row'}>
      <span className="set-row-body">
        <span className="set-label">{label}</span>
        {sub && <span className="set-sub">{sub}</span>}
      </span>
      {children}
      <Switch label={aria ?? label} on={on} disabled={disabled} onToggle={onToggle} />
    </div>
  )
}

export function FoldRow({
  label,
  value,
  sub,
  open,
  onToggle,
  children,
}: {
  label: string
  value?: string
  sub?: string
  open: boolean
  onToggle: () => void
  children: ReactNode
}) {
  return (
    <div className="set-fold">
      <button className="set-row set-open" aria-expanded={open} onClick={onToggle}>
        <span className="set-row-body">
          <span className="set-label">{label}</span>
          {sub && <span className="set-sub">{sub}</span>}
        </span>
        {value && <span className="set-value">{value}</span>}
        <Chevron />
      </button>
      {children}
    </div>
  )
}

export function Chevron() {
  return (
    <svg className="set-chev" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9 6l6 6-6 6" />
    </svg>
  )
}
