export type MicClient = 'web' | 'desktop'
export type MicKind = 'element' | MicClient
export type Mic = { client: string; kind: MicKind; label: string; gain_db: number; auto: boolean }
export type MicPatch = { gain_db?: number; auto?: boolean }
export type MicTrack = { deviceId?: string; groupId?: string; label: string }

// Chrome names whichever input is the default "default" or "communications", the same on every machine.
const ALIASES = new Set(['', 'default', 'communications'])

// The physical device behind a capture track, which may only know itself by an alias.
export function realDevice(track: MicTrack, inputs: MicTrack[]): MicTrack {
  if (!ALIASES.has(track.deviceId ?? '')) return track
  const real = inputs.filter((d) => !ALIASES.has(d.deviceId ?? ''))
  const same =
    real.find((d) => track.groupId && d.groupId === track.groupId) ??
    real.find((d) => track.label && (d.label === track.label || track.label.endsWith(d.label)))
  if (same) return { deviceId: same.deviceId, groupId: same.groupId, label: same.label }
  return { deviceId: track.groupId || undefined, groupId: track.groupId, label: track.label }
}

export const GAIN_MAX_DB = 24
const KEY_HEX = 16
const LABEL_MAX = 64

export async function micKey(track: MicTrack): Promise<string> {
  const bytes = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(track.deviceId || track.label))
  return [...new Uint8Array(bytes)]
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
    .slice(0, KEY_HEX)
}

export const micFrame = (client: MicClient, key: string, label: string): string =>
  JSON.stringify({ type: 'mic', client, key, label: [...label].slice(0, LABEL_MAX).join('') })

export const clampGain = (db: number) => Math.min(GAIN_MAX_DB, Math.max(0, Math.round(db)))

export const slid = (rows: Mic[], client: string, db: number): Mic[] =>
  rows.map((m) => (m.client === client ? { ...m, gain_db: clampGain(db), auto: false } : m))

export const autoOn = (rows: Mic[], client: string): Mic[] =>
  rows.map((m) => (m.client === client ? { ...m, auto: true } : m))

// Settings writes the gains it shows and a call names the device it captures from, so a slider moves the call's ring live.
const gains = new Map<string, number>()
let current: string | null = null

export function rememberGains(rows: Mic[]) {
  for (const m of rows) gains.set(m.client, m.gain_db)
}

// The server names a device `<client>:<key>`.
export const micClientId = (client: MicClient, key: string) => `${client}:${key}`

export function setCurrentMic(id: string | null) {
  current = id
}

export function gainScale(): number {
  const db = current ? gains.get(current) : undefined
  return db === undefined ? 1 : 10 ** (db / 20)
}
