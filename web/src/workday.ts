// The server's work day: it runs on past midnight until `dayEnd`, so the small
// hours still count toward the day before and read past 24:00.

const pad = (n: number) => String(n).padStart(2, '0')

export const wallMinutes = (wall: string): number => {
  const [h, m] = wall.split(':')
  return Number(h) * 60 + Number(m)
}

const lateNight = (at: Date, dayEnd: string) => at.getHours() * 60 + at.getMinutes() < wallMinutes(dayEnd)

export function workDate(at: Date, dayEnd: string): string {
  const d = new Date(at)
  if (lateNight(at, dayEnd)) d.setDate(d.getDate() - 1)
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

export function workMinutes(at: Date, dayEnd: string): number {
  const civil = at.getHours() * 60 + at.getMinutes() + at.getSeconds() / 60
  return lateNight(at, dayEnd) ? civil + 24 * 60 : civil
}

// Where the day line ends: midnight, or past it to `dayEnd`.
export const dayLineEnd = (dayEnd: string): number => 24 * 60 + wallMinutes(dayEnd)
