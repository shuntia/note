import { describe, expect, test } from 'vitest'
import { fill, t } from '.'
import { en } from './en'
import * as format from './format'

describe('fill', () => {
  test('fills named places and leaves unknown ones visible', () => {
    expect(fill('Mark {text} done', { text: 'milk' })).toBe('Mark milk done')
    expect(fill('{a} and {b}', { a: 1 })).toBe('1 and {b}')
  })

  test('picks the plural form for the count, with # as the number', () => {
    const blocks = '{count, one {# block} other {# blocks}} still open.'
    expect(fill(blocks, { count: 1 })).toBe('1 block still open.')
    expect(fill(blocks, { count: 3 })).toBe('3 blocks still open.')
    expect(fill(blocks, { count: 0 })).toBe('0 blocks still open.')
    expect(fill('{n, =0 {none} other {#}}', { n: 0 })).toBe('none')
  })

  test('mixes places and plurals in one string', () => {
    expect(fill('{done} of {count, one {# step} other {# steps}} done', { done: 1, count: 2 })).toBe('1 of 2 steps done')
  })
})

test('t reads the English dictionary', () => {
  expect(t('closeDay.open', { count: 2 })).toBe('2 blocks still open.')
  expect(t('nav.today')).toBe(en['nav.today'])
})

test('every English string is non-empty', () => {
  expect(Object.entries(en).filter(([, v]) => v.trim() === '')).toEqual([])
})

describe('format', () => {
  test('a day carries the year only when it is not the current one', () => {
    const now = new Date(2026, 8, 30)
    expect(format.day(new Date(2026, 8, 28), now)).toBe('Sep 28')
    expect(format.day(new Date(2025, 11, 31), now)).toBe('Dec 31, 2025')
  })

  test('the 24-hour clock pads the hour', () => {
    expect(format.clock24(new Date(2026, 8, 30, 9, 5))).toBe('09:05')
  })

  test('numbers follow the locale', () => {
    expect(format.number(0.25, { style: 'percent' })).toBe('25%')
  })
})
