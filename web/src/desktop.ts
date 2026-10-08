// What the Electron shell's preload exposes; absent in a browser.
export type NoteDesktop = { ring(): void }

export const desktop = (): NoteDesktop | undefined =>
  (window as Window & { noteDesktop?: NoteDesktop }).noteDesktop

// The desktop app rings from the tray, so it counts as in view while hidden; a browser tab does not.
export const ringable = (): boolean => desktop() !== undefined || !document.hidden
