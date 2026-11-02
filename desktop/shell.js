'use strict'

const TITLE_MAX = 80

/** A ring's details from the page; null for a page that predates the popup and expects the window shown. */
function ringInfo(raw) {
  if (!raw || typeof raw !== 'object') return null
  const title = typeof raw.title === 'string' ? [...raw.title.trim()].slice(0, TITLE_MAX).join('') : ''
  return { title: title || null }
}

function cornerBounds(area, size, margin) {
  return {
    x: Math.round(area.x + area.width - size.width - margin),
    y: Math.round(area.y + margin),
    ...size,
  }
}

const MIC_SETTINGS = {
  darwin: 'x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone',
  win32: 'ms-settings:privacy-microphone',
}

const micSettingsUrl = (platform) => MIC_SETTINGS[platform] ?? null

// Centred across, with its top a sixth of the way down: the input sits in the upper third and replies grow below it.
function centerTop(area, size) {
  return {
    x: Math.round(area.x + (area.width - size.width) / 2),
    y: Math.round(area.y + area.height / 6),
    ...size,
  }
}

const LAUNCHER_HEIGHT = { min: 72, max: 560 }

const launcherHeight = (asked) =>
  Number.isFinite(asked) ? Math.min(LAUNCHER_HEIGHT.max, Math.max(LAUNCHER_HEIGHT.min, Math.ceil(asked))) : LAUNCHER_HEIGHT.min

const ZOOM_MIN = -3
const ZOOM_MAX = 5

const clampZoom = (level) => (Number.isFinite(level) ? Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, level)) : 0)

/** The wanted spellcheck languages the platform has, matched by base language (`ja` takes `ja-JP`). */
function spellLanguages(available, wanted) {
  const picked = []
  for (const want of wanted) {
    const hit = available.find((a) => a === want) ?? available.find((a) => a.split('-')[0] === want.split('-')[0])
    if (hit && !picked.includes(hit)) picked.push(hit)
  }
  return picked
}

module.exports = { LAUNCHER_HEIGHT, centerTop, clampZoom, cornerBounds, launcherHeight, micSettingsUrl, ringInfo, spellLanguages }
