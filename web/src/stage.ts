import { useCallback, useRef, useState, type KeyboardEvent, type TouchEvent, type WheelEvent } from 'react'

const SWIPE_PX = 40
const WHEEL_PX = 60
const COOLDOWN_MS = 500

// A surface that reveals itself in steps: swipe up, wheel down or ArrowDown
// go one step further; the opposite gestures come back.
export function useStage(max: number) {
  const [stage, setStage] = useState(0)
  const touchY = useRef<number | null>(null)
  const wheel = useRef(0)
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

  const onWheel = (e: WheelEvent) => {
    wheel.current += e.deltaY
    if (wheel.current > WHEEL_PX) {
      wheel.current = 0
      step(1)
    } else if (wheel.current < -WHEEL_PX) {
      wheel.current = 0
      step(-1)
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
    else if (to - from > SWIPE_PX) step(-1)
  }
  const onKeyDown = (e: KeyboardEvent) => {
    const el = e.target as HTMLElement | null
    if (el?.tagName === 'INPUT' || el?.tagName === 'TEXTAREA') return
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      step(1)
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      step(-1)
    }
  }

  return { stage, setStage, bind: { onWheel, onTouchStart, onTouchEnd, onKeyDown, tabIndex: -1 } }
}
