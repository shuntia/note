import { expect, test, vi } from 'vitest'
import { modelName, NO_PICK, pickMenu, picks, shortModel, type ModelOffer } from './models'

const OFFER: ModelOffer = {
  default: 'deepseek/deepseek-v4-flash',
  models: ['deepseek/deepseek-v4-flash', 'deepseek/deepseek-v4.1-flash', 'other/deepseek-v4-flash'].map((id) => ({ id, name: '', desc: '' })),
  efforts: ['off', 'low', 'medium', 'high'],
  effort_default: 'low',
}

test('a model reads as the part after its last slash unless that is ambiguous', () => {
  expect(shortModel('deepseek/deepseek-v4.1-flash')).toBe('deepseek-v4.1-flash')
  expect(shortModel('plain')).toBe('plain')
  const ids = OFFER.models.map((m) => m.id)
  expect(shortModel('deepseek/deepseek-v4-flash', ids)).toBe('deepseek/deepseek-v4-flash')
  expect(shortModel('deepseek/deepseek-v4.1-flash', ids)).toBe('deepseek-v4.1-flash')
})

test('the menu marks the held model and effort and defaults when none is held', () => {
  const [model, effort] = pickMenu(OFFER, { model: 'deepseek/deepseek-v4.1-flash', effort: null }, () => {})
  expect(model.children?.filter((c) => c.checked).map((c) => c.label)).toEqual(['deepseek-v4.1-flash'])
  expect(model.children).toHaveLength(4)
  expect(effort.children?.[0].checked).toBe(true)
  expect(effort.children).toHaveLength(5)
})

test('choosing changes only its own half of the pick', () => {
  const choose = vi.fn()
  const [model, effort] = pickMenu(OFFER, { model: 'other/deepseek-v4-flash', effort: 'high' }, choose)
  effort.children?.[1].run?.()
  expect(choose).toHaveBeenLastCalledWith({ model: 'other/deepseek-v4-flash', effort: 'off' })
  model.children?.[0].run?.()
  expect(choose).toHaveBeenLastCalledWith({ model: null, effort: 'high' })
})

test('one model on offer leaves only the effort to choose', () => {
  const items = pickMenu({ ...OFFER, default: null, models: [] }, NO_PICK, () => {})
  expect(items).toHaveLength(1)
  expect(picks(NO_PICK)).toBe(false)
  expect(picks({ model: null, effort: 'off' })).toBe(true)
})

test('a configured name and description are what the menu shows', () => {
  const named: ModelOffer = {
    ...OFFER,
    models: [
      { id: 'deepseek/deepseek-v4.1-flash', name: 'DeepSeek Flash', desc: 'fast, cheap' },
      { id: 'x/thinker', name: '', desc: 'deep reasoning' },
    ],
  }
  const [model] = pickMenu(named, NO_PICK, () => {})
  expect(model.children?.slice(1).map((c) => [c.label, c.hint])).toEqual([
    ['DeepSeek Flash', 'fast, cheap'],
    ['thinker', 'deep reasoning'],
  ])
  expect(modelName('x/gone', named.models)).toBe('gone')
})
