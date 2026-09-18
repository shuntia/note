import type { CounterMode } from './session'
import type { Settings } from './types'

const KEY = 'note.prefs'

export type Prefs = { counter: CounterMode; showArc: boolean; closeDay: string }

const DEFAULT: Prefs = { counter: 'remaining', showArc: true, closeDay: '21:30' }

export function readPrefs(): Prefs {
  try {
    const raw = localStorage.getItem(KEY)
    if (!raw) return DEFAULT
    const p = JSON.parse(raw) as Partial<Prefs>
    return {
      counter: p.counter === 'elapsed' ? 'elapsed' : 'remaining',
      showArc: p.showArc !== false,
      closeDay: typeof p.closeDay === 'string' ? p.closeDay : DEFAULT.closeDay,
    }
  } catch {
    return DEFAULT
  }
}

export function writePrefs(p: Prefs) {
  try {
    localStorage.setItem(KEY, JSON.stringify(p))
  } catch {
    // storage blocked; the choice still holds for this session
  }
}

export function prefsFrom(
  s: Pick<Settings, 'counter' | 'show_arc_between_sessions' | 'close_day_time'>,
): Prefs {
  return { counter: s.counter, showArc: s.show_arc_between_sessions, closeDay: s.close_day_time }
}
