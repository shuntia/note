import { expect, test } from 'vitest'
import { fill, setLocale, t, type Key } from '.'
import { en } from './en'
import { ja } from './ja'

/** Names of the top-level `{name}` and `{name, …}` places in a template. */
function places(template: string): string[] {
  const names: string[] = []
  let depth = 0
  let start = -1
  for (let i = 0; i < template.length; i++) {
    if (template[i] === '{') {
      if (depth++ === 0) start = i + 1
    } else if (template[i] === '}' && --depth === 0) {
      names.push(template.slice(start, i).split(',')[0].trim())
    }
  }
  return [...new Set(names)].sort()
}

test('every Japanese string fills the same places as English', () => {
  for (const key of Object.keys(en) as Key[]) {
    expect(places(ja[key]), key).toEqual(places(en[key]))
  }
})

test('no Japanese string is empty', () => {
  for (const [key, value] of Object.entries(ja)) expect(value.trim(), key).not.toBe('')
})

test('t speaks Japanese once the locale is ja', () => {
  setLocale('ja')
  try {
    expect(t('closeDay.open', { count: 3 })).toBe('まだ3個のブロックが残っています。')
    expect(fill(ja['tasks.stepsDone'], { done: 1, count: 4 })).toBe('4ステップ中1完了')
  } finally {
    setLocale('en')
  }
})
