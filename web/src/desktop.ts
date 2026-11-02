// What the Electron shell's preload exposes; absent in a browser.
// Absent from builds older than the method.
import type { WidgetClock } from './session'

// `launcher` is the shortcut that opens the launcher, null when another app holds it.
export type ShellSettings = { startAtLogin: boolean; nightly: boolean | null; launcher?: string | null }
export type ShellAbout = { version: string; server: string | null }
export type RingAction = 'answer' | 'decline'
// `status` is the OS's own say over the mic where it keeps one; `settings` is whether its privacy page can be opened.
export type MicAccess = {
  status: 'not-determined' | 'granted' | 'denied' | 'restricted' | 'unknown'
  settings: boolean
}
export type NoteDesktop = {
  ring(info?: { title?: string }): void
  ringStop?(): void
  onRingAction?(fn: (action: RingAction) => void): () => void
  show?(): void
  settings?(): Promise<ShellSettings | null>
  set?(key: keyof ShellSettings, on: boolean): Promise<ShellSettings | null>
  changeServer?(): void
  inCall?(on: boolean): void
  widget?(): void
  widgetClock?(clock: WidgetClock | null): void
  shown?(): Promise<boolean | null>
  onShown?(fn: (shown: boolean) => void): () => void
  micAccess?(): Promise<MicAccess | null>
  micSettings?(): void
  onOpen?(fn: (path: string) => void): () => void
  onResume?(fn: () => void): () => void
  about?(): Promise<ShellAbout | null>
}

// The desktop app's launcher window, which loads this page in its own mode.
export type NoteLauncher = {
  hide(): void
  resize(height: number): void
  open(conversationId: number | null): void
  onShown(fn: () => void): () => void
}

export const launcherShell = (): NoteLauncher | undefined =>
  (window as Window & { noteLauncher?: NoteLauncher }).noteLauncher

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

// A notice the window is not showing becomes a system notification; a click brings the window up, at `open` if given.
export function systemNotice(title: string, body: string, open?: () => void): void {
  if (typeof Notification === 'undefined' || (!document.hidden && document.hasFocus())) return
  try {
    const n = new Notification(title, { body, silent: true })
    n.onclick = () => {
      desktop()?.show?.()
      open?.()
    }
  } catch {}
}

// Rings the desktop app's popup until the returned stop; `title` names the call once it is known. A shell from
// before the popup raises its window instead and has nothing to stop.
export function desktopRing(keys: Record<RingAction, () => void>, title?: Promise<string | null>): () => void {
  const shell = desktop()
  if (!shell) return () => {}
  if (!shell.ringStop) {
    shell.ring()
    return () => {}
  }
  let ringing = true
  shell.ring({})
  void title?.then(
    (name) => {
      if (ringing && name) shell.ring({ title: name })
    },
    () => {},
  )
  const stop = shell.onRingAction?.((action) => keys[action]())
  return () => {
    ringing = false
    stop?.()
    shell.ringStop?.()
  }
}
