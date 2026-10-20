import { t } from '../../i18n'

export const UNLIMITED = -1
export const MAX_TRIGGERS_PER_DAY = 20

export const triggersLabel = (n: number) =>
  n === UNLIMITED ? t('settings.triggers.unlimited') : t('settings.triggers.value', { n })

/** A cleared field is no limit; anything else is held to the range the server takes. */
export const triggersFromInput = (raw: string) =>
  raw === '' ? UNLIMITED : Math.min(MAX_TRIGGERS_PER_DAY, Math.max(UNLIMITED, Math.round(Number(raw) || 0)))
