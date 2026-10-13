import type { ViewProps } from '../../app'
import * as format from '../../i18n/format'

export type Notify = ViewProps['notify']

export const dayOf = (iso: string) => format.day(new Date(iso))
