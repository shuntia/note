import { useEffect, useRef, useState, type FocusEvent } from 'react'

export type OverflowItem = { label: string; run: () => void; disabled?: boolean }

export function Overflow({
  label,
  items,
  className = 'ev-more-wrap',
}: {
  label: string
  items: OverflowItem[]
  className?: string
}) {
  const [open, setOpen] = useState(false)
  const wrap = useRef<HTMLDivElement>(null)
  const trigger = useRef<HTMLButtonElement>(null)
  const first = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    if (!open) return
    first.current?.focus()
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return
      setOpen(false)
      trigger.current?.focus()
    }
    const onDown = (e: MouseEvent) => {
      if (!wrap.current?.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('keydown', onKey)
    document.addEventListener('mousedown', onDown)
    return () => {
      document.removeEventListener('keydown', onKey)
      document.removeEventListener('mousedown', onDown)
    }
  }, [open])

  // Focus moving between the menu's own items must not close it.
  const onBlur = (e: FocusEvent<HTMLDivElement>) => {
    if (!wrap.current?.contains(e.relatedTarget as Node | null)) setOpen(false)
  }

  return (
    <div className={className} ref={wrap}>
      <button
        className="ev-more"
        ref={trigger}
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        ⋯
      </button>
      {open && (
        <div className="ev-menu" role="menu" onBlur={onBlur}>
          {items.map((item, i) => (
            <button
              key={item.label}
              className="ev-menu-item"
              role="menuitem"
              ref={i === 0 ? first : undefined}
              disabled={item.disabled}
              onClick={() => {
                setOpen(false)
                item.run()
              }}
            >
              {item.label}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}
