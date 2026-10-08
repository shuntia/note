// What the Electron shell's preload exposes; absent in a browser.
// Absent from builds older than the method.
export type ShellSettings = { startAtLogin: boolean; nightly: boolean | null }
export type NoteDesktop = {
  ring(): void
  show?(): void
  settings?(): Promise<ShellSettings | null>
  set?(key: keyof ShellSettings, on: boolean): Promise<ShellSettings | null>
  changeServer?(): void
}

export const desktop = (): NoteDesktop | undefined =>
  (window as Window & { noteDesktop?: NoteDesktop }).noteDesktop

const CALLS_KEY = 'note.desktop.calls'
export const RINGABLE_CHANGED = 'note:ringable'

const stored = (key: string): boolean => {
  try {
    return localStorage.getItem(key) !== 'off'
  } catch {
    return true
  }
}

const store = (key: string, on: boolean) => {
  try {
    localStorage.setItem(key, on ? 'on' : 'off')
  } catch {}
}

export const callsOn = (): boolean => stored(CALLS_KEY)

export function setCallsOn(on: boolean): void {
  store(CALLS_KEY, on)
  window.dispatchEvent(new Event(RINGABLE_CHANGED))
}

// The desktop app rings from the tray, so it counts as in view while hidden unless its calls are off; a browser tab
// rings only while in view.
export const ringable = (): boolean => (desktop() ? callsOn() : !document.hidden)

const NOTICES_KEY = 'note.desktop.notices'

// The desktop app's own notices, on unless turned off; it takes the place of Web Push, which Electron cannot do.
export const noticesOn = (): boolean => stored(NOTICES_KEY)

export async function setNoticesOn(on: boolean): Promise<void> {
  if (on && typeof Notification !== 'undefined' && Notification.permission !== 'granted') await Notification.requestPermission()
  store(NOTICES_KEY, on)
}

// A notice the window is not showing becomes a system notification; a click brings the window up.
export function systemNotice(title: string, body: string): void {
  if (typeof Notification === 'undefined' || (!document.hidden && document.hasFocus())) return
  try {
    const n = new Notification(title, { body, silent: true })
    n.onclick = () => desktop()?.show?.()
  } catch {}
}
