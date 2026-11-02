import { expect, test } from 'vitest'
import type { Task } from '../types'
import { chipOf } from './Tasks'

const task = (category: string) => ({ category }) as Task

test('a row names its category while nothing over it already does', () => {
  expect(chipOf(task('school'), null, false)).toBe('school')
})

test('a row under its category heading or a picked pill goes without', () => {
  expect(chipOf(task('school'), null, true)).toBeNull()
  expect(chipOf(task('school'), 'school', false)).toBeNull()
})

test('a task with no category gets no chip', () => {
  expect(chipOf(task(''), null, false)).toBeNull()
})
