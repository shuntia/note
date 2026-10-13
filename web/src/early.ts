import { api } from './api'
import type { Me } from './types'

let early: Promise<Me> | null = null

// Starts the session check while the app's code is still downloading.
export function askMeEarly() {
  early = api.me()
  early.catch(() => {})
}

export function takeMe(): Promise<Me> {
  const answer = early ?? api.me()
  early = null
  return answer
}
