'use strict'

/** The address as an origin-rooted http(s) URL string, or null when it is not one; a bare host gets https://. */
function normalizeServerUrl(input) {
  const text = String(input ?? '').trim()
  if (!text) return null
  const withScheme = /^[a-z][a-z0-9+.-]*:/i.test(text) ? text : `https://${text}`
  let url
  try {
    url = new URL(withScheme)
  } catch {
    return null
  }
  if ((url.protocol !== 'http:' && url.protocol !== 'https:') || !url.hostname) return null
  return url.href
}

/** The first usable address of --url=, NOTE_URL, the saved setting and the build-baked one, with where it came from. */
function resolveServerUrl({ argv = [], env = {}, saved, baked }) {
  const arg = argv.find((a) => a.startsWith('--url='))?.slice('--url='.length)
  const candidates = [
    ['arg', arg],
    ['env', env.NOTE_URL],
    ['saved', saved],
    ['baked', baked],
  ]
  for (const [source, value] of candidates) {
    const url = normalizeServerUrl(value)
    if (url) return { url, source }
  }
  return null
}

module.exports = { normalizeServerUrl, resolveServerUrl }
