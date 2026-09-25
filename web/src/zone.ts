import type { Settings } from './types'

export const deviceZone = () => Intl.DateTimeFormat().resolvedOptions().timeZone

// A browser with no zone of its own reports UTC; that is not where anyone lives.
export const knownDeviceZone = (device: string, list: readonly string[]): boolean =>
  list.includes(device) && device !== 'UTC' && device !== 'GMT' && !device.startsWith('Etc/')

export function zoneChange(
  s: Pick<Settings, 'timezone' | 'timezone_auto' | 'timezones'>,
  device: string,
): { from: string; to: string } | null {
  if (!s.timezone_auto || device === s.timezone || !knownDeviceZone(device, s.timezones)) return null
  return { from: s.timezone, to: device }
}
