import type { Settings } from './types'

export const deviceZone = () => Intl.DateTimeFormat().resolvedOptions().timeZone

export function zoneChange(
  s: Pick<Settings, 'timezone' | 'timezone_auto' | 'timezones'>,
  device: string,
): { from: string; to: string } | null {
  if (!s.timezone_auto || device === s.timezone || !s.timezones.includes(device)) return null
  return { from: s.timezone, to: device }
}
