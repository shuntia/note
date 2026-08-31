export type ThemeChoice = 'system' | 'light' | 'dark'

const KEY = 'note.theme'

export function storedTheme(): ThemeChoice {
  try {
    const raw = localStorage.getItem(KEY)
    if (raw === 'system' || raw === 'light' || raw === 'dark') return raw
  } catch {
    // storage blocked; follow the system scheme
  }
  return 'system'
}

// No attribute means the media query decides, which is what "System" is.
export function applyTheme(choice: ThemeChoice) {
  const root = document.documentElement
  if (choice === 'system') root.removeAttribute('data-theme')
  else root.setAttribute('data-theme', choice)
}

export function saveTheme(choice: ThemeChoice) {
  try {
    localStorage.setItem(KEY, choice)
  } catch {
    // the choice still holds for this session
  }
}
