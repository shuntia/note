import type { Brief } from './types'

// Each browser or desktop window keeps its own: a brief put away here still greets the others once.
const SEEN_KEY = 'note.brief.seen'

export function briefSeen(brief: Brief): boolean {
  try {
    return localStorage.getItem(SEEN_KEY) === String(brief.id)
  } catch {
    return false
  }
}

export function markBriefSeen(brief: Brief): void {
  try {
    localStorage.setItem(SEEN_KEY, String(brief.id))
  } catch {
    // Without storage the brief greets this window again next time.
  }
}
