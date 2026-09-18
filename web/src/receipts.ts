type Args = Record<string, unknown>

const MAX_QUOTE = 80

const str = (a: Args, key: string): string =>
  typeof a[key] === 'string' ? (a[key] as string).trim() : ''

const num = (a: Args, key: string): number | null =>
  typeof a[key] === 'number' && Number.isFinite(a[key]) ? (a[key] as number) : null

function clip(text: string): string {
  const flat = text.replace(/\s+/g, ' ').trim()
  return flat.length > MAX_QUOTE ? `${flat.slice(0, MAX_QUOTE - 1)}…` : flat
}

const quoted = (text: string) => `“${clip(text)}”`

function span(minutes: number): string {
  const m = Math.abs(minutes)
  if (m < 60 || m % 60 !== 0) return `${m} min`
  return `${m / 60} hr`
}

export function eventLabel(kind: string): string {
  if (kind === 'debrief') return 'Morning debrief'
  if (kind === 'trigger') return 'Note checks in'
  const words = kind.replaceAll('_', ' ').replace('checkin', 'check-in').trim()
  return words.charAt(0).toUpperCase() + words.slice(1)
}

function dayWord(iso: string): string {
  const at = new Date(`${iso}T00:00`)
  if (Number.isNaN(at.getTime())) return iso
  const days = Math.round((at.setHours(0, 0, 0, 0) - new Date().setHours(0, 0, 0, 0)) / 86_400_000)
  if (days === 0) return 'today'
  if (days === 1) return 'tomorrow'
  return new Date(`${iso}T00:00`).toLocaleDateString(undefined, {
    weekday: 'short',
    month: 'short',
    day: 'numeric',
  })
}

const done: Record<string, (a: Args) => string> = {
  task_create: (a) => {
    const title = str(a, 'title')
    return title ? `Added ${quoted(title)} to your tasks` : 'Added a task'
  },
  task_update: (a) => {
    switch (str(a, 'state')) {
      case 'done':
        return 'Marked a task done'
      case 'dropped':
        return 'Dropped a task'
      case 'in_progress':
        return 'Started a task'
      case 'open':
        return 'Reopened a task'
    }
    const title = str(a, 'title')
    if (title) return `Renamed a task to ${quoted(title)}`
    if (str(a, 'notes')) return 'Added a note to a task'
    if (str(a, 'description')) return "Filled in a task's details"
    return 'Updated a task'
  },
  memory_query: (a) => {
    const q = str(a, 'query')
    return q ? `Searched memory for ${quoted(q)}` : 'Searched memory'
  },
  memory_read: () => 'Read a saved note',
  memory_write: (a) => {
    const summary = str(a, 'summary')
    const tail = summary ? `: ${clip(summary)}` : ''
    if (str(a, 'op') === 'update') return `Updated a saved note${tail}`
    if (str(a, 'op') === 'supersede') return `Replaced an older note${tail}`
    return `Remembered${tail}`
  },
  context_edit: (a) => {
    const append = str(a, 'append')
    return append
      ? `Added ${quoted(append)} to your background notes`
      : 'Updated your background notes'
  },
  schedule_slide: (a) => {
    const minutes = num(a, 'minutes')
    if (minutes === null || minutes === 0) return 'Moved an event'
    return `Moved an event ${span(minutes)} ${minutes < 0 ? 'earlier' : 'later'}`
  },
  schedule_snooze: (a) => {
    const minutes = num(a, 'minutes')
    return minutes === null ? 'Put an event off' : `Put an event off for ${span(minutes)}`
  },
  schedule_drop: () => 'Dropped an event from the day',
  schedule_insert: (a) => {
    const kind = str(a, 'kind')
    const name = kind ? quoted(eventLabel(kind)) : 'an event'
    const date = str(a, 'date')
    const time = str(a, 'time')
    const when = [date ? dayWord(date) : '', time ? `at ${time}` : ''].filter(Boolean).join(' ')
    return when ? `Added ${name} to ${when}` : `Added ${name} to the plan`
  },
  notify_send: (a) => {
    const text = str(a, 'text')
    return text ? `Sent you a nudge: ${clip(text)}` : 'Sent you a nudge'
  },
}

// What a call reads as while it is still running.
const running: Record<string, (a: Args) => string> = {
  task_create: () => 'Adding a task',
  task_update: () => 'Updating a task',
  memory_query: (a) => {
    const q = str(a, 'query')
    return q ? `Searching memory for ${quoted(q)}` : 'Searching memory'
  },
  memory_read: () => 'Opening a saved note',
  memory_write: () => 'Saving that to memory',
  context_edit: () => 'Updating your background notes',
  schedule_slide: () => 'Moving an event',
  schedule_snooze: () => 'Putting an event off',
  schedule_drop: () => 'Dropping an event',
  schedule_insert: () => 'Adding that to the plan',
  notify_send: () => 'Sending you a nudge',
}

const failed: Record<string, string> = {
  task_create: "Couldn't add that task",
  task_update: "Couldn't update that task",
  memory_query: "Couldn't search memory",
  memory_read: "Couldn't open that note",
  memory_write: "Couldn't save that to memory",
  context_edit: "Couldn't update your background notes",
  schedule_slide: "Couldn't move that event",
  schedule_snooze: "Couldn't put that event off",
  schedule_drop: "Couldn't drop that event",
  schedule_insert: "Couldn't add that to the plan",
  notify_send: "Couldn't send that nudge",
}

function parse(raw: string): Args {
  try {
    const value: unknown = JSON.parse(raw)
    return typeof value === 'object' && value !== null ? (value as Args) : {}
  } catch {
    return {}
  }
}

// The sentence for a call still in flight.
export function doing(name: string, args: string): string {
  const template = running[name]
  return template ? template(parse(args)) : `Using ${name}`
}

// The sentence for one tool call; an unrecognised tool still gets a sentence, never a raw block.
export function receipt(name: string, args: string, isError: boolean): string {
  if (isError) return failed[name] ?? `Couldn't use ${name}`
  const template = done[name]
  return template ? template(parse(args)) : `Used ${name}`
}
