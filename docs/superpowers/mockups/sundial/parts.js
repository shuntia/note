// Shared pieces for the frames: the arc, the tab bar, the desktop top bar.
export const arc = (frac = 0, { faded = false, size = 320, r = 148, w = 9 } = {}) => {
  const c = 2 * Math.PI * r
  const span = c * (240 / 360)
  return `<div class="arc" style="opacity:${faded ? 0.38 : 1}"><svg viewBox="0 0 ${size} ${size}">
    <circle cx="${size / 2}" cy="${size / 2}" r="${r}" fill="none" stroke="var(--track)" stroke-width="${w}" stroke-linecap="round" stroke-dasharray="${span.toFixed(1)} ${c.toFixed(1)}" transform="rotate(150 ${size / 2} ${size / 2})"/>
    ${frac > 0 ? `<circle cx="${size / 2}" cy="${size / 2}" r="${r}" fill="none" stroke="var(--arc-sun)" stroke-width="${w}" stroke-linecap="round" stroke-dasharray="${(span * frac).toFixed(1)} ${c.toFixed(1)}" transform="rotate(150 ${size / 2} ${size / 2})"/>` : ''}
  </svg></div>`
}

const ICONS = {
  Today: '<circle cx="12" cy="12" r="4"/><path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M5.6 18.4 7 17M17 7l1.4-1.4"/>',
  Tasks: '<path d="M4 6h1M4 12h1M4 18h1M9 6h11M9 12h11M9 18h11"/>',
  Chat: '<path d="M4 6a3 3 0 0 1 3-3h10a3 3 0 0 1 3 3v8a3 3 0 0 1-3 3H9l-5 4z"/>',
  Memory: '<path d="M9 18h6M10 21h4M12 3a6 6 0 0 0-3.5 10.9c.8.6 1.5 1.6 1.5 2.6V17h4v-.5c0-1 .7-2 1.5-2.6A6 6 0 0 0 12 3z"/>',
  Settings: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>',
}
export const tabs = (on = 'Today') =>
  `<nav class="tabs">${Object.entries(ICONS).map(([k, d]) => `<span class="tab${k === on ? ' on' : ''}"><svg viewBox="0 0 24 24">${d}</svg>${k}</span>`).join('')}</nav>`

export const topnav = (on = 'Today') =>
  `<div class="topnav"><span class="brand">Note</span><div class="links">${Object.keys(ICONS).map((k) => `<span class="${k === on ? 'on' : ''}">${k}</span>`).join('')}</div><div class="jot">＋ Jot anything<kbd>N</kbd></div></div>`

export const mount = (sel, html) => { document.querySelector(sel).innerHTML = html }
