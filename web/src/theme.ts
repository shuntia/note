export type ThemeChoice = 'system' | 'light' | 'dark'

// index.html reads this key in its pre-paint script; keep the two in step.
const KEY = 'note.theme'

// Installed-PWA chrome follows the page, sourced from the token so there is no second
// copy of the palette.
function paintChrome() {
  const meta = document.querySelector('meta[name="theme-color"]')
  const earth = getComputedStyle(document.documentElement).getPropertyValue('--earth').trim()
  if (meta && earth) meta.setAttribute('content', earth)
}

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
  paintChrome()
}

export function saveTheme(choice: ThemeChoice) {
  try {
    localStorage.setItem(KEY, choice)
  } catch {
    // the choice still holds for this session
  }
}
