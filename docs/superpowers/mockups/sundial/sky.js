// Reference implementation for the Sky theme: solar altitude → palette. The app's
// theme.ts is meant to lift this as-is.

const RAD = Math.PI / 180

// Solar altitude in degrees above the horizon (USNO low-precision algorithm,
// good to about a tenth of a degree, which is a minute or so of twilight).
export function solarAltitude(date, lat, lon) {
  const d = (date.getTime() - Date.UTC(2000, 0, 1, 12)) / 86400000
  const g = (357.529 + 0.98560028 * d) * RAD
  const q = 280.459 + 0.98564736 * d
  const L = (q + 1.915 * Math.sin(g) + 0.02 * Math.sin(2 * g)) * RAD
  const e = (23.439 - 0.00000036 * d) * RAD
  const ra = Math.atan2(Math.cos(e) * Math.sin(L), Math.cos(L)) / RAD
  const dec = Math.asin(Math.sin(e) * Math.sin(L))
  const gmst = (18.697374558 + 24.06570982441908 * d) % 24
  const h = ((gmst * 15 + lon - ra) % 360 + 540) % 360 - 180
  return Math.asin(Math.sin(lat * RAD) * Math.sin(dec) + Math.cos(lat * RAD) * Math.cos(dec) * Math.cos(h * RAD)) / RAD
}

// The zone's offset from UTC in hours at an instant.
export function zoneOffsetHours(date, zone) {
  const parts = new Intl.DateTimeFormat('en-US', { timeZone: zone, timeZoneName: 'longOffset' }).formatToParts(date)
  const off = parts.find((p) => p.type === 'timeZoneName')?.value ?? 'GMT'
  const m = /GMT([+-])(\d{1,2})(?::(\d{2}))?/.exec(off)
  return m ? (m[1] === '-' ? -1 : 1) * (Number(m[2]) + Number(m[3] ?? 0) / 60) : 0
}

// Without a location the zone's standard offset gives the longitude (solar noon
// lands within the zone's width; daylight saving is left out, since it shifts the
// clock and not the sun) and a mid-latitude stands in for the rest: sunrise within
// about half an hour for most people.
export function placeFromZone(zone) {
  const y = new Date().getUTCFullYear()
  const standard = Math.min(zoneOffsetHours(new Date(Date.UTC(y, 0, 1)), zone), zoneOffsetHours(new Date(Date.UTC(y, 6, 1)), zone))
  return { lat: 35, lon: standard * 15 }
}

// The keyframes, as oklch triples plus alpha, keyed like the app's tokens.
const K = {
  day: {
    'sky-top': [84, 0.035, 230], 'sky-mid': [91, 0.025, 200], earth: [93.5, 0.016, 78],
    haze: [99, 0.004, 80, 0.55], 'haze-strong': [99, 0.004, 80, 0.86],
    ink: [23, 0.03, 255], ivory: [98, 0.008, 85], quiet: [40, 0.025, 250], faint: [45, 0.02, 245],
    line: [30, 0.03, 250, 0.45], track: [30, 0.03, 250, 0.16], 'sun-ink': [45, 0.13, 55],
    sage: [50, 0.09, 150], rose: [55, 0.07, 30],
  },
  golden: {
    'sky-top': [74, 0.07, 250], 'sky-mid': [86, 0.09, 72], earth: [89, 0.035, 62],
    haze: [99, 0.006, 80, 0.5], 'haze-strong': [98, 0.008, 80, 0.84],
    ink: [24, 0.035, 275], ivory: [98, 0.01, 85], quiet: [40, 0.03, 270], faint: [46, 0.03, 265],
    line: [32, 0.04, 270, 0.45], track: [32, 0.04, 270, 0.17], 'sun-ink': [44, 0.14, 48],
    sage: [50, 0.09, 150], rose: [55, 0.08, 28],
  },
  dusk: {
    'sky-top': [30, 0.07, 285], 'sky-mid': [48, 0.11, 30], earth: [30, 0.03, 45],
    haze: [40, 0.03, 45, 0.55], 'haze-strong': [36, 0.03, 45, 0.9],
    ink: [94, 0.014, 80], ivory: [28, 0.03, 45], quiet: [80, 0.02, 75], faint: [72, 0.02, 70],
    line: [92, 0.01, 80, 0.35], track: [92, 0.01, 80, 0.15], 'sun-ink': [82, 0.13, 80],
    sage: [74, 0.08, 150], rose: [74, 0.07, 30],
  },
  night: {
    'sky-top': [23, 0.03, 258], 'sky-mid': [21, 0.022, 240], earth: [20, 0.018, 60],
    haze: [30, 0.02, 60, 0.6], 'haze-strong': [28, 0.02, 60, 0.92],
    ink: [93, 0.012, 85], ivory: [20, 0.02, 60], quiet: [74, 0.015, 80], faint: [66, 0.014, 75],
    line: [90, 0.01, 80, 0.35], track: [90, 0.01, 80, 0.14], 'sun-ink': [78, 0.13, 78],
    sage: [72, 0.08, 150], rose: [70, 0.06, 30],
  },
}

// Text, lines and surfaces never blend between a light set and a dark one: they
// switch where the ground crosses the middle, so contrast holds through twilight.
const STEPPED = new Set(['ink', 'ivory', 'quiet', 'faint', 'line', 'track', 'sun-ink', 'sage', 'rose', 'haze', 'haze-strong'])

// Altitude bands. Above 12° the sky is plain day; the golden hour hangs on the
// horizon; civil and nautical twilight are dusk; astronomical night below -10°.
export function phaseAt(alt) {
  if (alt >= 12) return { from: 'day', to: 'day', t: 0 }
  if (alt >= 4) return { from: 'golden', to: 'day', t: (alt - 4) / 8 }
  if (alt >= -2) return { from: 'dusk', to: 'golden', t: (alt + 2) / 6 }
  if (alt >= -10) return { from: 'night', to: 'dusk', t: (alt + 10) / 8 }
  return { from: 'night', to: 'night', t: 0 }
}

const smooth = (t) => t * t * (3 - 2 * t)
const lerpHue = (a, b, t) => {
  let d = ((b - a + 540) % 360) - 180
  return (a + d * t + 360) % 360
}
function mix(a, b, t) {
  return [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, lerpHue(a[2], b[2], t), a[3] === undefined ? undefined : a[3] + (b[3] - a[3]) * t]
}
const css = ([l, c, h, a]) => `oklch(${l.toFixed(2)}% ${c.toFixed(4)} ${h.toFixed(1)}${a === undefined ? '' : ` / ${a.toFixed(3)}`})`

// All tokens for an altitude, plus which colour-scheme the stepped set is on.
export function paletteAt(alt) {
  const { from, to, t } = phaseAt(alt)
  const tt = smooth(t)
  // the ground crosses the middle during dusk↔golden; text flips there
  const dark = alt < 1
  const out = {}
  for (const key of Object.keys(K.day)) {
    if (STEPPED.has(key)) out[key] = css(dark ? K[alt < -6 ? 'night' : 'dusk'][key] : K[alt < 8 ? 'golden' : 'day'][key])
    else out[key] = css(mix(K[from][key], K[to][key], tt))
  }
  return { tokens: out, dark, phase: t < 0.5 ? from : to }
}

export function applyPalette(el, alt) {
  const { tokens, dark, phase } = paletteAt(alt)
  for (const [k, v] of Object.entries(tokens)) el.style.setProperty(`--${k}`, v)
  el.style.colorScheme = dark ? 'dark' : 'light'
  el.dataset.phase = phase
  return { dark, phase }
}
