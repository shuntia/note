import { beforeEach, expect, test, vi } from 'vitest'
import { briefSeen, markBriefSeen } from './brief'
import type { Brief } from './types'

const brief = (id: number): Brief => ({ id, date: '2026-11-26', content: 'A light day.', created_at: '2026-11-25T22:00:00Z' })

beforeEach(() => {
  const store = new Map<string, string>()
  vi.stubGlobal('localStorage', {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
  })
})

test('a brief greets this window until it is put away, and the next one greets it again', () => {
  expect(briefSeen(brief(1))).toBe(false)
  markBriefSeen(brief(1))
  expect(briefSeen(brief(1))).toBe(true)
  expect(briefSeen(brief(2))).toBe(false)
})

test('without storage the brief still shows and putting it away does not throw', () => {
  vi.stubGlobal('localStorage', {
    getItem: () => {
      throw new Error('denied')
    },
    setItem: () => {
      throw new Error('denied')
    },
  })
  expect(briefSeen(brief(1))).toBe(false)
  expect(() => markBriefSeen(brief(1))).not.toThrow()
})
