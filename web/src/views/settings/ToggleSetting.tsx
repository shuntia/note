import { createContext, useContext } from 'react'
import type { SettingsPatch, WritableSetting } from '../../api'
import type { Save } from '../../hooks'
import type { AlertPatch, Settings, SettingsSaved } from '../../types'
import { Status, ToggleRow } from '../../ui/rows'

export type SaveOptions = {
  alerts?: AlertPatch[]
  // A success clears the row's status instead of showing it saved.
  quiet?: boolean
  // Runs on the server's answer before it lands; true stops there.
  after?: (saved: SettingsSaved) => boolean | void
}

export type SaveSettings = (row: string, patch: SettingsPatch, opts?: SaveOptions) => Promise<boolean>

export const SettingsContext = createContext<{
  settings: Settings
  save: Save
  busy: boolean
  saveSettings: SaveSettings
} | null>(null)

type BooleanSetting = { [K in WritableSetting]: Settings[K] extends boolean ? K : never }[WritableSetting]

/**
 * A switch row that saves `field` on toggle. Its status shows inline under the field's
 * name, or under `row` where that row shows it.
 */
export function ToggleSetting({
  field,
  label,
  sub,
  aria,
  row,
  quiet,
}: {
  field: BooleanSetting
  label: string
  sub?: string
  aria?: string
  row?: string
  quiet?: boolean
}) {
  const ctx = useContext(SettingsContext)
  if (!ctx) return null
  const on = ctx.settings[field]
  return (
    <ToggleRow
      label={label}
      sub={sub}
      aria={aria}
      on={on}
      disabled={ctx.busy}
      onToggle={() => void ctx.saveSettings(row ?? field, { [field]: !on }, { quiet })}
    >
      {row === undefined && <Status save={ctx.save} row={field} />}
    </ToggleRow>
  )
}
