import { t, type Key } from '../../i18n'
import type { RingFor } from '../../types'

export const RING_CHOICES: { id: RingFor; key: Key }[] = [
  { id: 'allowed', key: 'settings.ring.allowed' },
  { id: 'never', key: 'settings.ring.never' },
]

export function RingChoices({
  value,
  disabled,
  onPick,
}: {
  value: RingFor
  disabled?: boolean
  onPick: (choice: RingFor) => void
}) {
  return (
    <div className="seg" role="group" aria-label={t('settings.ring.label')}>
      {RING_CHOICES.map((c) => (
        <button
          key={c.id}
          type="button"
          aria-pressed={value === c.id}
          disabled={disabled}
          onClick={() => onPick(c.id)}
        >
          {t(c.key)}
        </button>
      ))}
    </div>
  )
}
