import type { CounterMode } from './session'
import type { Settings } from './types'

const KEY = 'note.prefs'

export type Prefs = {
  counter: CounterMode
  showArc: boolean
  closeDay: string
  dayEnd: string
  soonDays: number
}

const DEFAULT: Prefs = { counter: 'remaining', showArc: true, closeDay: '21:30', dayEnd: '00:00', soonDays: 7 }

const count = (v: unknown, fallback: number) => (typeof v === 'number' && v > 0 ? v : fallback)

export function readPrefs(): Prefs {
  try {
    const raw = localStorage.getItem(KEY)
    if (!raw) return DEFAULT
    const p = JSON.parse(raw) as Partial<Prefs>
    return {
      counter: p.counter === 'elapsed' ? 'elapsed' : 'remaining',
      showArc: p.showArc !== false,
      closeDay: typeof p.closeDay === 'string' ? p.closeDay : DEFAULT.closeDay,
      dayEnd: typeof p.dayEnd === 'string' && /^\d\d:\d\d$/.test(p.dayEnd) ? p.dayEnd : DEFAULT.dayEnd,
      soonDays: count(p.soonDays, DEFAULT.soonDays),
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
  s: Pick<Settings, 'counter' | 'show_arc_between_sessions' | 'close_day_time'> & {
    tuning?: Pick<Settings['tuning'], 'day_end' | 'soon_days'>
  },
): Prefs {
  return {
    counter: s.counter,
    showArc: s.show_arc_between_sessions,
    closeDay: s.close_day_time,
    dayEnd: s.tuning?.day_end ?? DEFAULT.dayEnd,
    soonDays: s.tuning?.soon_days ?? DEFAULT.soonDays,
  }
}
