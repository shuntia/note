import { useState } from 'react'
import { doubleColumn, setDoubleColumn } from '../../columns'
import { t } from '../../i18n'
import { ToggleRow } from '../../ui/rows'

export function AppearanceSection() {
  const [on, setOn] = useState(doubleColumn)
  return (
    <div className="set-fold-body">
      <ToggleRow
        label={t('settings.doubleColumn')}
        on={on}
        onToggle={() => {
          setOn(!on)
          setDoubleColumn(!on)
        }}
      />
    </div>
  )
}
