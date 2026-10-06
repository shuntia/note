import { activeLocale } from '.'

/** 9:05 AM, or the locale's own way of writing a clock time. */
export function clock(at: Date): string {
  return at.toLocaleTimeString(activeLocale(), { hour: 'numeric', minute: '2-digit' })
}

/** 09:05, the 24-hour clock whatever the locale's habit. */
export function clock24(at: Date): string {
  return at.toLocaleTimeString(activeLocale(), { hour: '2-digit', minute: '2-digit', hour12: false })
}

/** Sep 30, with the year only when it is not this year's. */
export function day(at: Date, now: Date = new Date()): string {
  return at.toLocaleDateString(activeLocale(), {
    month: 'short',
    day: 'numeric',
    ...(at.getFullYear() === now.getFullYear() ? {} : { year: 'numeric' }),
  })
}

/** Sep 30, 2026. */
export function fullDay(at: Date): string {
  return at.toLocaleDateString(activeLocale(), { month: 'short', day: 'numeric', year: 'numeric' })
}

/** Wednesday, September 30. */
export function longDay(at: Date): string {
  return at.toLocaleDateString(activeLocale(), { weekday: 'long', month: 'long', day: 'numeric' })
}

/** Wed, Sep 30. */
export function weekdayDay(at: Date): string {
  return at.toLocaleDateString(activeLocale(), { weekday: 'short', month: 'short', day: 'numeric' })
}

export function weekday(at: Date, width: 'long' | 'short' | 'narrow' = 'short'): string {
  return at.toLocaleDateString(activeLocale(), { weekday: width })
}

export function month(at: Date, width: 'long' | 'short' = 'long'): string {
  return at.toLocaleDateString(activeLocale(), { month: width })
}

export function number(n: number, options?: Intl.NumberFormatOptions): string {
  return new Intl.NumberFormat(activeLocale(), options).format(n)
}

/** "in 5 minutes", "2 days ago". */
export function relative(value: number, unit: Intl.RelativeTimeFormatUnit): string {
  return new Intl.RelativeTimeFormat(activeLocale(), { numeric: 'auto' }).format(value, unit)
}
