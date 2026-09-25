import { paletteAt, placeFromZone, solarAltitude, TOKENS, type Place } from './sky'

export type ThemeChoice = 'system' | 'light' | 'dark' | 'sky'

// index.html reads these keys in its pre-paint script; keep the two in step.
const KEY = 'note.theme'
const SKY_KEY = 'note.sky'
const PLACE_KEY = 'note.place'

// Installed-PWA chrome follows the page, sourced from the token so there is no second
// copy of the palette.
function paintChrome() {
  const meta = document.querySelector('meta[name="theme-color"]')
  const earth = getComputedStyle(document.documentElement).getPropertyValue('--earth').trim()
  if (meta && earth) meta.setAttribute('content', earth)
}

export const deviceZone = () => Intl.DateTimeFormat().resolvedOptions().timeZone

export function storedPlace(): Place | null {
  try {
    const raw = localStorage.getItem(PLACE_KEY)
    if (!raw) return null
    const p = JSON.parse(raw)
    return typeof p?.lat === 'number' && typeof p?.lon === 'number' ? { lat: p.lat, lon: p.lon } : null
  } catch {
    return null
  }
}

export function savePlace(p: Place | null) {
  try {
    if (p) localStorage.setItem(PLACE_KEY, JSON.stringify(p))
    else localStorage.removeItem(PLACE_KEY)
  } catch {}
}

export const currentPlace = (): Place => storedPlace() ?? placeFromZone(deviceZone())

let skyTimer: number | undefined

function stopSky() {
  if (skyTimer !== undefined) window.clearInterval(skyTimer)
  skyTimer = undefined
  document.removeEventListener('visibilitychange', paintSky)
  const root = document.documentElement
  for (const t of TOKENS) root.style.removeProperty(`--${t}`)
  root.style.removeProperty('color-scheme')
  root.removeAttribute('data-scheme')
}

// Writes the palette for this minute inline on :root, and keeps it for the pre-paint.
// Does nothing unless Sky is the active theme and the page is visible.
export function paintSky() {
  const root = document.documentElement
  if (root.getAttribute('data-theme') !== 'sky' || document.hidden) return
  const place = currentPlace()
  const { tokens, dark } = paletteAt(solarAltitude(new Date(), place.lat, place.lon))
  for (const [k, v] of Object.entries(tokens)) root.style.setProperty(`--${k}`, v)
  root.style.colorScheme = dark ? 'dark' : 'light'
  root.setAttribute('data-scheme', dark ? 'dark' : 'light')
  try {
    localStorage.setItem(SKY_KEY, JSON.stringify({ tokens, dark }))
  } catch {
    // the next load paints from the stylesheet until the bundle repaints
  }
  paintChrome()
}

export function storedTheme(): ThemeChoice {
  try {
    const raw = localStorage.getItem(KEY)
    if (raw === 'system' || raw === 'light' || raw === 'dark' || raw === 'sky') return raw
  } catch {
    // storage blocked; follow the system scheme
  }
  return 'system'
}

// No attribute means the media query decides, which is what "System" is.
export function applyTheme(choice: ThemeChoice) {
  const root = document.documentElement
  stopSky()
  if (choice === 'system') root.removeAttribute('data-theme')
  else root.setAttribute('data-theme', choice)
  if (choice === 'sky') {
    paintSky()
    skyTimer = window.setInterval(paintSky, 60_000)
    document.addEventListener('visibilitychange', paintSky)
  }
  paintChrome()
}

export function saveTheme(choice: ThemeChoice) {
  try {
    localStorage.setItem(KEY, choice)
  } catch {
    // the choice still holds for this session
  }
}
