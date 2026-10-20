import { t, type Key } from './i18n'
import type { OverflowItem } from './overflow'

export type Effort = 'off' | 'low' | 'medium' | 'high'

export const EFFORTS: Effort[] = ['off', 'low', 'medium', 'high']

// `default` is null on a provider without a model name, such as the built-in mock.
export type ModelOffer = {
  default: string | null
  models: string[]
  efforts: Effort[]
  effort_default: Effort | null
}

// Null halves follow the next in line: a chat's pick, then Settings, then the server.
export type ModelPick = { model: string | null; effort: Effort | null }

export const NO_PICK: ModelPick = { model: null, effort: null }

export const effortKey = (e: Effort): Key => `effort.${e}`

// `deepseek/deepseek-v4.1-flash` reads as `deepseek-v4.1-flash`, unless another model on offer reads the same.
export function shortModel(id: string, among: string[] = []) {
  const short = (m: string) => m.slice(m.lastIndexOf('/') + 1)
  return among.some((m) => m !== id && short(m) === short(id)) ? id : short(id)
}

export const picks = (p: ModelPick) => p.model !== null || p.effort !== null

// A single model on offer is no choice, so its group is left out.
export function pickMenu(offer: ModelOffer, pick: ModelPick, choose: (next: ModelPick) => void): OverflowItem[] {
  const fallback = t('talk.pick.default')
  const items: OverflowItem[] = []
  if (offer.models.length > 1)
    items.push({
      label: t('talk.pick.model'),
      children: [
        { label: fallback, checked: pick.model === null, run: () => choose({ ...pick, model: null }) },
        ...offer.models.map((m) => ({
          label: shortModel(m, offer.models),
          checked: pick.model === m,
          run: () => choose({ ...pick, model: m }),
        })),
      ],
    })
  items.push({
    label: t('talk.pick.effort'),
    children: [
      { label: fallback, checked: pick.effort === null, run: () => choose({ ...pick, effort: null }) },
      ...offer.efforts.map((e) => ({
        label: t(effortKey(e)),
        checked: pick.effort === e,
        run: () => choose({ ...pick, effort: e }),
      })),
    ],
  })
  return items
}
