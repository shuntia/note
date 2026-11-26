import { useState, type ReactNode } from 'react'
import {
  applyAppearance,
  DENSITIES,
  FONTS,
  MOTIONS,
  saveAppearance,
  storedAppearance,
  TEXT_SIZES,
  type Appearance,
} from '../../appearance'
import { doubleColumn, setDoubleColumn } from '../../columns'
import { t, type Key } from '../../i18n'
import { ToggleRow } from '../../ui/rows'

const TEXT_GLYPH_EM = { small: 0.8, normal: 1, large: 1.2, larger: 1.4 } as const

function MotionGlyph({ id }: { id: Appearance['motion'] }) {
  return (
    <svg viewBox="0 0 24 16" aria-hidden="true">
      {id === 'system' && (
        <>
          <circle cx="12" cy="8" r="5" className="look-stroke" />
          <path d="M12 3a5 5 0 0 1 0 10z" />
        </>
      )}
      {id === 'reduced' && <circle cx="12" cy="8" r="3.5" />}
      {id === 'full' && (
        <>
          <circle cx="16" cy="8" r="3.5" />
          <path d="M3 8h6M5 4.5h5M5 11.5h5" className="look-stroke" />
        </>
      )}
    </svg>
  )
}

function DensityGlyph({ id }: { id: Appearance['density'] }) {
  const ys = id === 'compact' ? [4, 7, 10, 13] : [3, 8, 13]
  return (
    <svg viewBox="0 0 24 16" aria-hidden="true">
      {ys.map((y) => (
        <path key={y} d={`M5 ${y}h14`} className="look-stroke" />
      ))}
    </svg>
  )
}

function Choice<T extends string>({
  label,
  all,
  value,
  name,
  glyph,
  onPick,
}: {
  label: Key
  all: readonly T[]
  value: T
  name: (id: T) => Key
  glyph: (id: T) => ReactNode
  onPick: (id: T) => void
}) {
  return (
    <div className="set-row look-row">
      <span className="set-row-body">
        <span className="set-label">{t(label)}</span>
      </span>
      <div className="seg look-seg" role="group" aria-label={t(label)}>
        {all.map((id) => (
          <button
            key={id}
            type="button"
            aria-pressed={value === id}
            aria-label={t(name(id))}
            title={t(name(id))}
            onClick={() => onPick(id)}
          >
            {glyph(id)}
          </button>
        ))}
      </div>
    </div>
  )
}

export function AppearanceSection() {
  const [on, setOn] = useState(doubleColumn)
  const [look, setLook] = useState(storedAppearance)
  const pick = (patch: Partial<Appearance>) => {
    const next = { ...look, ...patch }
    setLook(next)
    applyAppearance(next)
    saveAppearance(next)
  }
  return (
    <div className="set-fold-body">
      <Choice
        label="settings.motion"
        all={MOTIONS}
        value={look.motion}
        name={(id) => `settings.motion.${id}`}
        glyph={(id) => <MotionGlyph id={id} />}
        onPick={(motion) => pick({ motion })}
      />
      <Choice
        label="settings.font"
        all={FONTS}
        value={look.font}
        name={(id) => `settings.font.${id}`}
        glyph={(id) => <span style={{ fontFamily: `var(--font-${id})` }}>{t('settings.font.glyph')}</span>}
        onPick={(font) => pick({ font })}
      />
      <Choice
        label="settings.textSize"
        all={TEXT_SIZES}
        value={look.text}
        name={(id) => `settings.textSize.${id}`}
        glyph={(id) => <span style={{ fontSize: `${TEXT_GLYPH_EM[id]}em` }}>{t('settings.textSize.glyph')}</span>}
        onPick={(text) => pick({ text })}
      />
      <Choice
        label="settings.density"
        all={DENSITIES}
        value={look.density}
        name={(id) => `settings.density.${id}`}
        glyph={(id) => <DensityGlyph id={id} />}
        onPick={(density) => pick({ density })}
      />
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
