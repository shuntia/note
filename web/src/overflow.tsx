import {
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type FocusEvent,
  type ReactNode,
  type Ref,
} from 'react'
import { createPortal } from 'react-dom'
import gsap from 'gsap'
import { popIn, popOut } from './motion-gsap'
import { reducedMotion } from './motion'

// `checked` makes the item one of a choice: the menu shows which one holds.
// `children` makes it a group: one row until it is asked for, then radio rows under it.
export type OverflowItem = {
  label: string
  run?: () => void
  disabled?: boolean
  checked?: boolean
  kind?: 'action' | 'danger'
  children?: OverflowItem[]
}

export type OverflowHandle = {
  /** Opens the menu at a point in the viewport, clamped inside it. */
  openAt: (x: number, y: number) => void
}

const POINTER = '(min-width: 768px) and (pointer: fine)'
const LONG_PRESS_MS = 500
const LONG_PRESS_SLOP = 10
const EDGE = 8

/** True where the menu belongs at the bottom of the screen rather than under its trigger. */
export function useMenuSheet(): boolean {
  const [sheet, setSheet] = useState(() => !window.matchMedia(POINTER).matches)
  useEffect(() => {
    const mq = window.matchMedia(POINTER)
    const read = () => setSheet(!mq.matches)
    read()
    mq.addEventListener('change', read)
    return () => mq.removeEventListener('change', read)
  }, [])
  return sheet
}

// A menu sitting above its trigger leaves and returns downward.
const opensUp = (menu: HTMLElement | null, trigger: HTMLElement | null) =>
  !!menu && !!trigger && menu.getBoundingClientRect().top < trigger.getBoundingClientRect().top

const classes = (...names: (string | false | undefined)[]) => names.filter(Boolean).join(' ')

export function Overflow({
  label,
  items,
  className = 'ev-more-wrap',
  title,
  subtitle,
  row,
  trigger: showTrigger = true,
  ref: handle,
}: {
  label: string
  items: OverflowItem[]
  className?: string
  title?: string
  subtitle?: string
  // A selector for the row this menu belongs to: a right-click anywhere on it, or a
  // still press held half a second, opens the menu where the pointer is.
  row?: string
  trigger?: boolean
  ref?: Ref<OverflowHandle>
}) {
  const sheet = useMenuSheet()
  const [open, setOpen] = useState(false)
  const [closing, setClosing] = useState(false)
  const [group, setGroup] = useState<string | null>(null)
  const [point, setPoint] = useState<{ x: number; y: number } | null>(null)
  const wrap = useRef<HTMLDivElement>(null)
  const trigger = useRef<HTMLButtonElement>(null)
  const menu = useRef<HTMLDivElement>(null)
  const scrim = useRef<HTMLDivElement>(null)

  const close = useCallback(() => setClosing(true), [])
  const inside = (n: Node | null) =>
    !!n && (!!wrap.current?.contains(n) || !!menu.current?.contains(n))

  const openAt = useCallback(
    (x: number, y: number) => {
      setPoint(sheet ? null : { x, y })
      setClosing(false)
      setOpen(true)
    },
    [sheet],
  )
  useImperativeHandle(handle, () => ({ openAt }), [openAt])

  useEffect(() => {
    if (!row) return
    const el = wrap.current?.closest<HTMLElement>(row)
    if (!el) return
    let timer = 0
    let held = 0
    let from: { x: number; y: number } | null = null
    // The press that opened the menu must not also press what is under it.
    const swallow = (e: Event) => {
      e.preventDefault()
      e.stopPropagation()
    }
    const forget = () => {
      window.clearTimeout(held)
      removeEventListener('click', swallow, true)
    }
    const stop = () => {
      window.clearTimeout(timer)
      from = null
    }
    const onMenu = (e: MouseEvent) => {
      stop()
      e.preventDefault()
      openAt(e.clientX, e.clientY)
    }
    const onDown = (e: PointerEvent) => {
      if (e.pointerType !== 'touch') return
      from = { x: e.clientX, y: e.clientY }
      timer = window.setTimeout(() => {
        from = null
        forget()
        addEventListener('click', swallow, { capture: true, once: true })
        held = window.setTimeout(() => removeEventListener('click', swallow, true), 600)
        openAt(e.clientX, e.clientY)
      }, LONG_PRESS_MS)
    }
    const onMove = (e: PointerEvent) => {
      if (!from) return
      if (
        Math.abs(e.clientX - from.x) > LONG_PRESS_SLOP ||
        Math.abs(e.clientY - from.y) > LONG_PRESS_SLOP
      )
        stop()
    }
    el.addEventListener('contextmenu', onMenu)
    el.addEventListener('pointerdown', onDown)
    el.addEventListener('pointermove', onMove)
    el.addEventListener('pointerup', stop)
    el.addEventListener('pointercancel', stop)
    return () => {
      stop()
      forget()
      el.removeEventListener('contextmenu', onMenu)
      el.removeEventListener('pointerdown', onDown)
      el.removeEventListener('pointermove', onMove)
      el.removeEventListener('pointerup', stop)
      el.removeEventListener('pointercancel', stop)
    }
  }, [row, openAt])

  useLayoutEffect(() => {
    const el = menu.current
    if (!open || sheet || !point || !el) return
    const r = el.getBoundingClientRect()
    el.style.left = `${Math.max(EDGE, Math.min(point.x, innerWidth - r.width - EDGE))}px`
    el.style.top = `${Math.max(EDGE, Math.min(point.y, innerHeight - r.height - EDGE))}px`
  }, [open, sheet, point, group])

  const step = useCallback((by: number) => {
    const rows = Array.from(
      menu.current?.querySelectorAll<HTMLButtonElement>('.ev-menu-item:not([disabled])') ?? [],
    )
    if (rows.length === 0) return
    const at = rows.indexOf(document.activeElement as HTMLButtonElement)
    rows[(Math.max(at, 0) + by + rows.length) % rows.length]?.focus()
  }, [])

  useEffect(() => {
    if (!open) return
    if (sheet) {
      if (!reducedMotion()) {
        gsap.from(scrim.current, { autoAlpha: 0, duration: 0.3, ease: 'power2.out' })
        gsap.from(menu.current, {
          y: 48,
          autoAlpha: 0,
          duration: 0.4,
          ease: 'expo.out',
          clearProps: 'transform,opacity,visibility',
        })
      }
    } else {
      popIn(menu.current, opensUp(menu.current, trigger.current))
    }
    menu.current?.querySelector<HTMLButtonElement>('.ev-menu-item:not([disabled])')?.focus()
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        close()
        trigger.current?.focus()
        return
      }
      if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return
      e.preventDefault()
      step(e.key === 'ArrowDown' ? 1 : -1)
    }
    const onDown = (e: MouseEvent) => {
      if (!inside(e.target as Node)) close()
    }
    document.addEventListener('keydown', onKey)
    if (!sheet) document.addEventListener('mousedown', onDown)
    return () => {
      document.removeEventListener('keydown', onKey)
      document.removeEventListener('mousedown', onDown)
    }
  }, [open, sheet, close, step])

  useEffect(() => {
    if (!closing) return
    const settled = () => {
      setClosing(false)
      setOpen(false)
      setGroup(null)
      setPoint(null)
    }
    if (!sheet) {
      popOut(menu.current, settled, opensUp(menu.current, trigger.current))
      return
    }
    if (reducedMotion()) {
      settled()
      return
    }
    gsap.to(scrim.current, { autoAlpha: 0, duration: 0.25, ease: 'power2.in' })
    gsap.to(menu.current, {
      y: 48,
      autoAlpha: 0,
      duration: 0.28,
      ease: 'power2.in',
      onComplete: settled,
    })
  }, [closing, sheet])

  // Reopening mid-close: coming back in kills the tween that would unmount it.
  const toggle = () => {
    if (closing) {
      setClosing(false)
      if (!sheet) popIn(menu.current, opensUp(menu.current, trigger.current))
      else if (!reducedMotion()) {
        gsap.killTweensOf([scrim.current, menu.current])
        gsap.to([scrim.current, menu.current], {
          autoAlpha: 1,
          y: 0,
          duration: 0.2,
          ease: 'power2.out',
        })
      }
    } else if (open) close()
    else {
      setPoint(null)
      setOpen(true)
    }
  }

  // Focus moving between the menu's own items must not close it.
  const onBlur = (e: FocusEvent<HTMLDivElement>) => {
    if (!inside(e.relatedTarget as Node | null)) close()
  }

  const pick = (item: OverflowItem) => {
    if (item.children) {
      setGroup((held) => (held === item.label ? null : item.label))
      return
    }
    close()
    item.run?.()
  }

  const rows = (list: OverflowItem[], child = false): ReactNode[] =>
    list.flatMap((item) => {
      const expanded = group === item.label
      const held = item.children?.find((one) => one.checked)
      const row = (
        <button
          key={item.label}
          className={classes(
            'ev-menu-item',
            child && 'ev-menu-child',
            item.children && 'ev-menu-group',
            item.kind === 'danger' && 'danger',
          )}
          role={item.children || item.checked === undefined ? 'menuitem' : 'menuitemradio'}
          aria-checked={item.children ? undefined : item.checked}
          aria-expanded={item.children ? expanded : undefined}
          disabled={item.disabled}
          onClick={() => pick(item)}
        >
          <span className="ev-menu-label">{item.label}</span>
          {item.children && (
            <>
              {held && <span className="ev-menu-held">{held.label}</span>}
              <span className="ev-menu-chev" aria-hidden="true">
                ›
              </span>
            </>
          )}
        </button>
      )
      return expanded && item.children ? [row, ...rows(item.children, true)] : [row]
    })

  const plain = items.filter((item) => item.kind !== 'danger')
  const danger = items.filter((item) => item.kind === 'danger')
  const body = (
    <>
      {(title || subtitle) && (
        <div className="ev-menu-head">
          {title && <span className="ev-menu-head-title">{title}</span>}
          {subtitle && <span className="ev-menu-head-sub">{subtitle}</span>}
        </div>
      )}
      {rows(plain)}
      {danger.length > 0 && <div className="ev-menu-sep" />}
      {rows(danger)}
    </>
  )

  const popover = open && !sheet && (
    <div
      className="ev-menu"
      role="menu"
      ref={menu}
      onBlur={onBlur}
      style={point ? { position: 'fixed', left: point.x, top: point.y, right: 'auto', zIndex: 60 } : undefined}
    >
      {body}
    </div>
  )

  return (
    <div className={className} ref={wrap} data-open={open && !closing}>
      {showTrigger && (
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
      )}
      {popover && (point ? createPortal(popover, document.body) : popover)}
      {open &&
        sheet &&
        createPortal(
          <>
            <div className="scrim" ref={scrim} onClick={close} />
            <div className="sheet menu-sheet" role="menu" aria-label={label} ref={menu}>
              <div className="sheet-handle" />
              {body}
            </div>
          </>,
          document.body,
        )}
    </div>
  )
}
