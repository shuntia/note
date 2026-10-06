import { activeLocale } from '.'

/** 9:05 AM, or the locale's own way of writing a clock time. */
export function clock(at: Date): string {
  return at.toLocaleTimeString(activeLocale(), { hour: 'numeric', minute: '2-digit' })
}

/** 09:05, the 24-hour clock whatever the locale's habit. */
export function clock24(at: Date): string {
  return at.toLocaleTimeString(activeLocale(), { hour: '2-digit', minute: '2-digit', hour12: false })
}

/** Sep 30, 2026, 9:05:12 AM. */
export function dateTime(at: Date): string {
  return at.toLocaleString(activeLocale())
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

/** "a, b and c" in the locale's own way of listing. */
export function list(items: string[]): string {
  return new Intl.ListFormat(activeLocale(), { type: 'conjunction' }).format(items)
}

/** Wednesday, September 30. */
export function longDay(at: Date): string {
  return at.toLocaleDateString(activeLocale(), { weekday: 'long', month: 'long', day: 'numeric' })
}

/** September 30. */
export function monthDay(at: Date): string {
  return at.toLocaleDateString(activeLocale(), { month: 'long', day: 'numeric' })
}

export function weekday(at: Date, width: 'long' | 'short' = 'short'): string {
  return at.toLocaleDateString(activeLocale(), { weekday: width })
}

export function number(n: number, options?: Intl.NumberFormatOptions): string {
  return new Intl.NumberFormat(activeLocale(), options).format(n)
}

/** "a, b, c", a short list run together the locale's way. */
export function unitList(items: string[]): string {
  return new Intl.ListFormat(activeLocale(), { style: 'short', type: 'unit' }).format(items)
}

/** "in 5 minutes", "2 days ago". */
export function relative(value: number, unit: Intl.RelativeTimeFormatUnit): string {
  return new Intl.RelativeTimeFormat(activeLocale(), { numeric: 'auto' }).format(value, unit)
}
