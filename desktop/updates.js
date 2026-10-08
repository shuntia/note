'use strict'

const RELEASES_URL = 'https://github.com/shuntia/note/releases'
const CHECK_EVERY_MS = 4 * 60 * 60 * 1000

/**
 * How this build gets a newer version: 'self' when electron-updater can replace it
 * (Windows NSIS, Linux AppImage), 'page' when the releases page is the way (macOS is
 * unsigned, a deb has no updater), null when something else owns it (dev, Nix).
 */
function updateMode({ platform, packaged, dev, env = {} }) {
  if (dev || !packaged || env.NOTE_DESKTOP_EXEC) return null
  if (platform === 'win32') return 'self'
  if (platform === 'linux') return env.APPIMAGE ? 'self' : 'page'
  if (platform === 'darwin') return 'page'
  return null
}

module.exports = { CHECK_EVERY_MS, RELEASES_URL, updateMode }
