import { useCallback, useEffect, useRef, useState, type FocusEvent } from 'react'
import { popIn, popOut } from './motion-gsap'

// `checked` makes the item one of a choice: the menu shows which one holds.
export type OverflowItem = {
  label: string
  run: () => void
  disabled?: boolean
  checked?: boolean
}

// A menu sitting above its trigger leaves and returns downward.
const opensUp = (menu: HTMLElement | null, trigger: HTMLElement | null) =>
  !!menu && !!trigger && menu.getBoundingClientRect().top < trigger.getBoundingClientRect().top

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
  const [closing, setClosing] = useState(false)
  const wrap = useRef<HTMLDivElement>(null)
  const trigger = useRef<HTMLButtonElement>(null)
  const menu = useRef<HTMLDivElement>(null)
  const first = useRef<HTMLButtonElement>(null)

  const close = useCallback(() => setClosing(true), [])

  useEffect(() => {
    if (!open) return
    popIn(menu.current, opensUp(menu.current, trigger.current))
    first.current?.focus()
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return
      close()
      trigger.current?.focus()
    }
    const onDown = (e: MouseEvent) => {
      if (!wrap.current?.contains(e.target as Node)) close()
    }
    document.addEventListener('keydown', onKey)
    document.addEventListener('mousedown', onDown)
    return () => {
      document.removeEventListener('keydown', onKey)
      document.removeEventListener('mousedown', onDown)
    }
  }, [open, close])

  useEffect(() => {
    if (!closing) return
    popOut(
      menu.current,
      () => {
        setClosing(false)
        setOpen(false)
      },
      opensUp(menu.current, trigger.current),
    )
  }, [closing])

  // Reopening mid-close: popping back in kills the tween that would unmount it.
  const toggle = () => {
    if (closing) {
      setClosing(false)
      popIn(menu.current, opensUp(menu.current, trigger.current))
    } else if (open) close()
    else setOpen(true)
  }

  // Focus moving between the menu's own items must not close it.
  const onBlur = (e: FocusEvent<HTMLDivElement>) => {
    if (!wrap.current?.contains(e.relatedTarget as Node | null)) close()
  }

  return (
    <div className={className} ref={wrap} data-open={open && !closing}>
      <button
        className="ev-more"
        ref={trigger}
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open && !closing}
        onClick={toggle}
      >
        ⋯
      </button>
      {open && (
        <div className="ev-menu" role="menu" ref={menu} onBlur={onBlur}>
          {items.map((item, i) => (
            <button
              key={item.label}
              className="ev-menu-item"
              role={item.checked === undefined ? 'menuitem' : 'menuitemradio'}
              aria-checked={item.checked}
              ref={i === 0 ? first : undefined}
              disabled={item.disabled}
              onClick={() => {
                close()
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
