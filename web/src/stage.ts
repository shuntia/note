import { useCallback, useRef, useState, type KeyboardEvent, type TouchEvent, type WheelEvent } from 'react'

const SWIPE_PX = 40
const WHEEL_PX = 60
const COOLDOWN_MS = 500
const WHEEL_IDLE_MS = 400

// A surface that reveals itself in steps: swipe up, wheel down or ArrowDown
// go one step further; the opposite gestures come back.
export function useStage(max: number) {
  const [stage, setStage] = useState(0)
  const touchY = useRef<number | null>(null)
  const wheel = useRef(0)
  const wheelAt = useRef(0)
  const lastAt = useRef(0)

  const step = useCallback(
    (delta: 1 | -1) => {
      const at = Date.now()
      if (at - lastAt.current < COOLDOWN_MS) return
      lastAt.current = at
      setStage((s) => Math.min(max, Math.max(0, s + delta)))
    },
    [max],
  )

  // The last stage can scroll, so it is left only from its top; a pause between
  // wheel events starts the count over rather than adding to the last gesture.
  const back = () => {
    if (stage === max && window.scrollY > 0) return
    step(-1)
  }

  const onWheel = (e: WheelEvent) => {
    const at = Date.now()
    if (at - wheelAt.current > WHEEL_IDLE_MS) wheel.current = 0
    wheelAt.current = at
    if (e.deltaY < 0 && window.scrollY > 0) return
    wheel.current += e.deltaY
    if (wheel.current > WHEEL_PX) {
      wheel.current = 0
      step(1)
    } else if (wheel.current < -WHEEL_PX) {
      wheel.current = 0
      back()
    }
  }
  const onTouchStart = (e: TouchEvent) => {
    touchY.current = e.touches[0]?.clientY ?? null
  }
  const onTouchEnd = (e: TouchEvent) => {
    const from = touchY.current
    touchY.current = null
    const to = e.changedTouches[0]?.clientY
    if (from === null || to === undefined) return
    if (from - to > SWIPE_PX) step(1)
    else if (to - from > SWIPE_PX) back()
  }
  const onKeyDown = (e: KeyboardEvent) => {
    const el = e.target as HTMLElement | null
    if (el?.tagName === 'INPUT' || el?.tagName === 'TEXTAREA') return
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      step(1)
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      back()
    }
  }

  return { stage, setStage, bind: { onWheel, onTouchStart, onTouchEnd, onKeyDown, tabIndex: -1 } }
}
