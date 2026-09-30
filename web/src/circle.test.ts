import { beforeEach, expect, test, vi } from 'vitest'
import { beadAt, C, hintShown, hintUsed, railX, segments, sessionFor, slotAfter, withoutCategory, WORK_TIME } from './circle'
import type { QueueEntry, Task } from './types'

const task = (o: Partial<Task>): Task =>
  ({ id: 1, title: 'Read', due_at: null, state: 'open', notes: '', duration_min: null, is_now: false, ...o }) as unknown as Task
const entry = (o: Partial<QueueEntry>): QueueEntry =>
  ({ task: { ...task({}), children: [] }, step: null, planned_min: null, reason: 'oldest', ...o })

beforeEach(() => {
  const store = new Map<string, string>()
  vi.stubGlobal('localStorage', {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
  })
})

test('beads sit evenly across the opening', () => {
  const a = beadAt(0, 4), d = beadAt(3, 4)
  expect(a.x).toBeGreaterThan(160); expect(d.x).toBeLessThan(160)
  expect(Math.hypot(a.x - 160, a.y - 160)).toBeCloseTo(148, 5)
  expect(a.y).toBeCloseTo(d.y, 5)
})

test('segments split the span with 5° gaps', () => {
  const s = segments(3, 1, 0.35)
  expect(s).toHaveLength(3)
  expect(s.map((x) => x.frac)).toEqual([1, 0.35, 0])
  expect(s[0].start).toBe(150)
  expect(s[1].start - s[0].start).toBeCloseTo((240 - 10) / 3 + 5, 5)
  expect(s.reduce((n, x) => n + x.len, 0)).toBeCloseTo((C * (240 - 10)) / 360, 3)
})

test('a release lands on the next slot at 60 px or a quick 20 px', () => {
  expect(slotAfter(1, -70, false, 4)).toBe(2)
  expect(slotAfter(1, -30, false, 4)).toBe(1)
  expect(slotAfter(1, -30, true, 4)).toBe(2)
  expect(slotAfter(0, 90, false, 4)).toBe(0)
  expect(slotAfter(3, -90, false, 4)).toBe(3)
})

test('the rail rubber-bands past the ends', () => {
  expect(railX(1, -40, 4)).toBe(-360)
  expect(railX(0, 100, 4)).toBe(30)
  expect(railX(3, -100, 4)).toBe(-990)
})

test('a session is built like the Tasks view builds one', () => {
  const step = task({ id: 9, title: 'Step two', notes: 'n', duration_min: 20 })
  const parent = { ...task({ id: 3, title: 'Essay' }), children: [task({ id: 8, state: 'done' }), step, task({ id: 10 })] }
  expect(sessionFor(entry({ task: parent, step, planned_min: 20 }))).toEqual({
    title: 'Essay', task_id: 3, notes: 'n', step_index: 2, step_count: 3, step_name: 'Step two', planned_min: 20,
  })
})

test('the category leaves the title and the step name', () => {
  const step = task({ id: 9, title: 'MIT — Draft essays 1-2', category: 'MIT' })
  const parent = { ...task({ id: 3, title: 'MIT — Draft 4 essays', category: 'MIT' }), children: [step] }
  expect(sessionFor(entry({ task: parent, step }))).toMatchObject({ title: 'Draft 4 essays', step_name: 'Draft essays 1-2' })
})

test('only a leading "category — " goes, and never the whole title', () => {
  expect(withoutCategory('MIT — Draft', 'MIT')).toBe('Draft')
  expect(withoutCategory('Draft for MIT', 'MIT')).toBe('Draft for MIT')
  expect(withoutCategory('MIT — ', 'MIT')).toBe('MIT — ')
  expect(withoutCategory('MIT — Draft', '')).toBe('MIT — Draft')
})

test('a task without steps carries its own notes and no step fields', () => {
  const parent = { ...task({ id: 4, title: 'Call', notes: 'ring' }), children: [] }
  expect(sessionFor(entry({ task: parent }))).toEqual({ title: 'Call', task_id: 4, notes: 'ring' })
})

test('work time is a bare title', () => {
  expect(WORK_TIME).toEqual({ title: 'Work time' })
})

test('each hint shows three times, counted separately', () => {
  expect(hintShown('start')).toBe(true)
  hintUsed('start'); hintUsed('start')
  expect(hintShown('start')).toBe(true)
  hintUsed('start')
  expect(hintShown('start')).toBe(false)
  expect(hintShown('session')).toBe(true)
  expect(localStorage.getItem('note.hints.start')).toBe('3')
})

test('an unreadable counter starts over, and a throwing store never hides the hint', () => {
  localStorage.setItem('note.hints.session', 'junk')
  expect(hintShown('session')).toBe(true)
  hintUsed('session')
  expect(localStorage.getItem('note.hints.session')).toBe('1')
  vi.stubGlobal('localStorage', { getItem: () => { throw new Error('denied') }, setItem: () => { throw new Error('denied') } })
  expect(hintShown('start')).toBe(true)
  expect(() => hintUsed('start')).not.toThrow()
})

test('a scheduled entry starts in its own block', () => {
  const parent = { ...task({ id: 4, title: 'Call', notes: 'ring' }), children: [] }
  expect(sessionFor(entry({ task: parent, planned_min: 25, reason: 'scheduled', event_id: 71 }))).toEqual({
    title: 'Call', task_id: 4, notes: 'ring', planned_min: 25, event_id: 71,
  })
})
