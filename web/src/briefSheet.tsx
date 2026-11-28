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

// The icon on the visible day line, which the sheet bows back into once the sun has set.
function anchor(): Element | null {
  return [...document.querySelectorAll('.dl-brief')].find((el) => el.getClientRects().length > 0) ?? null
}

// The brief fills the screen as a dawn of its own: the sky warms from the horizon, the sun lifts, and the words follow.
export function BriefSheet({ text, onClose }: { text: string; onClose: () => void }) {
  const sheet = useRef<HTMLDivElement>(null)
  const closing = useRef(false)

  useLayoutEffect(() => {
    const el = sheet.current
    if (reducedMotion() || !el) return
    const q = gsap.utils.selector(el)
    const rise = gsap.timeline({ defaults: { clearProps: 'transform,opacity,visibility' } })
    rise
      .from(el, { autoAlpha: 0, duration: 0.5, ease: 'power1.out' })
      .from(q('.brief-dawn'), { autoAlpha: 0, scaleY: 0.3, transformOrigin: '50% 100%', duration: 1.8, ease: 'power2.out' }, 0)
      .from(q('.brief-sun'), { yPercent: 70, autoAlpha: 0, duration: 2.2, ease: 'expo.out' }, 0.1)
      .from(q('.brief-body'), { y: 28, autoAlpha: 0, duration: 0.9, ease: 'power3.out' }, 0.55)
    return () => {
      rise.kill()
    }
  }, [])

  const close = useCallback(() => {
    if (closing.current) return
    closing.current = true
    const el = sheet.current
    if (reducedMotion() || !el) return onClose()
    const q = gsap.utils.selector(el)
    gsap
      .timeline({
        onComplete: () => {
          onClose()
          const icon = anchor()
          if (icon) gsap.fromTo(icon, { scale: 1.35 }, { scale: 1, duration: 0.5, ease: 'back.out(3)', clearProps: 'transform' })
        },
      })
      .to(q('.brief-body'), { y: -16, autoAlpha: 0, duration: 0.3, ease: 'power2.in' })
      .to(q('.brief-sun'), { yPercent: 70, duration: 0.6, ease: 'power2.in' }, 0)
      .to(q('.brief-dawn'), { autoAlpha: 0, duration: 0.5, ease: 'power1.in' }, 0)
      .to(el, { autoAlpha: 0, duration: 0.35, ease: 'power1.in' }, 0.25)
  }, [onClose])
  useEscape(true, close)

  // A tap anywhere puts it away, but not one meant for a link, a copy button or a selection.
  const tap = (e: { target: EventTarget }) => {
    if ((e.target as Element).closest('a, button') || window.getSelection()?.toString()) return
    close()
  }

  return createPortal(
    <div className="brief" role="dialog" aria-modal="true" aria-label={t('brief.open')} ref={sheet} onClick={tap}>
      <div className="brief-dawn" />
      <div className="brief-sun" />
      <div className="brief-body">
        <div className="brief-text">
          <SunriseIcon />
          <Markdown text={text} />
        </div>
      </div>
    </div>,
    document.body,
  )
}
