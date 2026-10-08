import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'
import { liftDrag, type DropTo } from './order'

type Fake = {
  dataset: Record<string, string>
  style: { transform: string }
  isConnected: boolean
  top: number
  height: number
  edge: boolean
}

const fake = (top: number, height: number, edge = false): Fake => ({
  dataset: {},
  style: { transform: '' },
  isConnected: true,
  top,
  height,
  edge,
})

// Three rows with the order's edge after the first, all inside one list.
function page() {
  const rows = [fake(0, 40), fake(53, 40), fake(93, 40)]
  const edge = fake(46, 1, true)
  const list = {
    dataset: {} as Record<string, string>,
    rows,
    querySelectorAll: (sel: string) =>
      sel === '[data-drag]' ? list.rows : [list.rows[0], edge, ...list.rows.slice(1)],
  }
  for (const r of rows)
    Object.assign(r, {
      closest: (sel: string) => (sel === '[data-drag]' ? r : sel === '[data-drag-list]' ? list : null),
      matches: (sel: string) => sel === '.order-edge' && r.edge,
      getBoundingClientRect: () => ({ top: r.top, height: r.height }),
    })
  Object.assign(edge, {
    matches: (sel: string) => sel === '.order-edge',
    getBoundingClientRect: () => ({ top: edge.top, height: edge.height }),
  })
  return { rows, list }
}

const fire = (doc: EventTarget, type: string, props: Record<string, unknown> = {}) => {
  const e = new Event(type, { cancelable: true })
  const { target, ...rest } = props
  Object.assign(e, { button: 0, pointerId: 1, clientX: 0, clientY: 0, ...rest })
  if (target) Object.defineProperty(e, 'target', { value: target })
  const stop = vi.spyOn(e, 'stopPropagation')
  doc.dispatchEvent(e)
  return { e, stop }
}

describe('liftDrag', () => {
  let doc: EventTarget
  let drop: ReturnType<typeof vi.fn<DropTo>>
  let off: () => void

  beforeEach(() => {
    vi.useFakeTimers()
    vi.stubGlobal('window', { matchMedia: () => ({ matches: true }) })
    doc = new EventTarget()
    drop = vi.fn<DropTo>(() => true)
    off = liftDrag(doc as Document, { current: drop })
  })
  afterEach(() => {
    off()
    vi.useRealTimers()
    vi.unstubAllGlobals()
  })

  const hold = (row: Fake, y: number) => {
    fire(doc, 'pointerdown', { target: row, clientY: y })
    vi.advanceTimersByTime(450)
  }

  test('a held row dragged over the edge commits once after settling', () => {
    const { rows } = page()
    hold(rows[2], 113)
    expect(rows[2].dataset.lifted).toBe('')
    fire(doc, 'pointermove', { clientY: 10 })
    fire(doc, 'pointerup', { clientY: 10 })
    expect(drop).toHaveBeenCalledWith({ from: 2, to: 0, into: true }, false)
    vi.runAllTimers()
    expect(drop).toHaveBeenLastCalledWith({ from: 2, to: 0, into: true }, true)
    expect(drop).toHaveBeenCalledTimes(2)
    expect(rows.map((r) => r.style.transform)).toEqual(['', '', ''])
    expect(rows[2].dataset.lifted).toBeUndefined()
  })

  test('a touch contextmenu mid-press is swallowed before the row menu sees it and lifts the row', () => {
    const { rows } = page()
    fire(doc, 'pointerdown', { target: rows[1], clientY: 73 })
    const { e, stop } = fire(doc, 'contextmenu')
    expect(e.defaultPrevented).toBe(true)
    expect(stop).toHaveBeenCalled()
    expect(rows[1].dataset.lifted).toBe('')
  })

  test('a contextmenu with no press is left alone', () => {
    const { e, stop } = fire(doc, 'contextmenu')
    expect(e.defaultPrevented).toBe(false)
    expect(stop).not.toHaveBeenCalled()
  })

  test('rows that changed under the drag are put back without a drop', () => {
    const { rows, list } = page()
    hold(rows[2], 113)
    fire(doc, 'pointermove', { clientY: 20 })
    list.rows = [rows[1], rows[0], rows[2]]
    fire(doc, 'pointerup', { clientY: 20 })
    vi.runAllTimers()
    expect(drop).not.toHaveBeenCalled()
    expect(rows.map((r) => r.style.transform)).toEqual(['', '', ''])
  })

  test('a drop that changes nothing eases back and never commits', () => {
    drop.mockReturnValue(false)
    const { rows } = page()
    hold(rows[1], 73)
    fire(doc, 'pointermove', { clientY: 130 })
    fire(doc, 'pointerup', { clientY: 130 })
    vi.runAllTimers()
    expect(drop).toHaveBeenCalledTimes(1)
    expect(drop).toHaveBeenCalledWith(expect.anything(), false)
    expect(rows.map((r) => r.style.transform)).toEqual(['', '', ''])
  })

  test('a second finger cancels a pending press', () => {
    const { rows } = page()
    fire(doc, 'pointerdown', { target: rows[1], clientY: 73 })
    fire(doc, 'pointerdown', { target: rows[2], clientY: 113, pointerId: 2 })
    vi.advanceTimersByTime(450)
    expect(rows[1].dataset.lifted).toBeUndefined()
    expect(rows[2].dataset.lifted).toBeUndefined()
  })

  test('another pointer cannot move or drop a lifted row', () => {
    const { rows } = page()
    hold(rows[2], 113)
    fire(doc, 'pointermove', { clientY: 20, pointerId: 2 })
    fire(doc, 'pointerup', { clientY: 20, pointerId: 2 })
    expect(rows[2].style.transform).toBe('scale(1.02)')
    expect(drop).not.toHaveBeenCalled()
  })

  test('teardown while settling cancels the commit and clears the rows', () => {
    const { rows } = page()
    hold(rows[2], 113)
    fire(doc, 'pointermove', { clientY: 20 })
    fire(doc, 'pointerup', { clientY: 20 })
    off()
    vi.runAllTimers()
    expect(drop).toHaveBeenCalledTimes(1)
    expect(rows[2].dataset.lifted).toBeUndefined()
    expect(rows.map((r) => r.style.transform)).toEqual(['', '', ''])
  })
})
