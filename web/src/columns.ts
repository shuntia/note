const KEY = 'note.doubleColumn'

// Whether this device lays the task list out in two columns where it has the width.
export function doubleColumn(): boolean {
  try {
    return localStorage.getItem(KEY) === 'on'
  } catch {
    return false
  }
}

export function setDoubleColumn(on: boolean): void {
  try {
    localStorage.setItem(KEY, on ? 'on' : 'off')
  } catch {
    // storage blocked; the choice holds until the page reloads
  }
}
