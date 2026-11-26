import { gsap } from 'gsap'
import { useCallback, useLayoutEffect, useRef } from 'react'
import { createPortal } from 'react-dom'
import { useEscape } from './escape'
import { t } from './i18n'
import { Markdown } from './markdown'
import { reducedMotion } from './motion'

export function SunriseIcon() {
  return (
    <svg className="sunrise-icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M7 17a5 5 0 0 1 10 0" />
      <path d="M3 17h18M12 6v2.5M5.6 9.6l1.7 1.7M18.4 9.6l-1.7 1.7M8 21h8" />
    </svg>
  )
}

// The icon on the visible day line, which the sheet rises from and folds back into.
function anchor(): Element | null {
  return [...document.querySelectorAll('.dl-brief')].find((el) => el.getClientRects().length > 0) ?? null
}

function offsetTo(card: Element, target: Element | null): { x: number; y: number } | null {
  if (!target) return null
  const a = card.getBoundingClientRect()
  const b = target.getBoundingClientRect()
  return { x: b.left + b.width / 2 - (a.left + a.width / 2), y: b.top + b.height / 2 - (a.top + a.height / 2) }
}

export function BriefSheet({ text, onClose }: { text: string; onClose: () => void }) {
  const scrim = useRef<HTMLDivElement>(null)
  const card = useRef<HTMLDivElement>(null)
  const closing = useRef(false)

  useLayoutEffect(() => {
    if (reducedMotion() || !card.current) return
    const from = offsetTo(card.current, anchor())
    gsap.from(scrim.current, { autoAlpha: 0, duration: 0.4, ease: 'power2.out' })
    gsap.from(card.current, {
      ...(from ?? { y: 24 }),
      scale: from ? 0.06 : 0.98,
      autoAlpha: 0,
      duration: 0.6,
      ease: 'expo.out',
      clearProps: 'transform,opacity,visibility',
    })
  }, [])

  const close = useCallback(() => {
    if (closing.current) return
    closing.current = true
    const icon = anchor()
    if (reducedMotion() || !card.current) return onClose()
    const to = offsetTo(card.current, icon)
    gsap.to(scrim.current, { autoAlpha: 0, duration: 0.35, ease: 'power2.in' })
    gsap.to(card.current, {
      ...(to ?? { y: 24 }),
      scale: to ? 0.06 : 0.98,
      autoAlpha: 0,
      duration: 0.42,
      ease: 'power3.in',
      onComplete: () => {
        onClose()
        if (icon) gsap.fromTo(icon, { scale: 1.35 }, { scale: 1, duration: 0.5, ease: 'back.out(3)', clearProps: 'transform' })
      },
    })
  }, [onClose])
  useEscape(true, close)

  // A tap anywhere on the card puts it away, but not one meant for a link, a copy button or a selection.
  const tap = (e: { target: EventTarget }) => {
    if ((e.target as Element).closest('a, button') || window.getSelection()?.toString()) return
    close()
  }

  return createPortal(
    <>
      <div className="scrim brief-scrim" ref={scrim} onClick={close} />
      <div className="brief-card" role="dialog" aria-modal="true" aria-label={t('brief.open')} ref={card} onClick={tap}>
        <SunriseIcon />
        <Markdown text={text} />
      </div>
    </>,
    document.body,
  )
}
