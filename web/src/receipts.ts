type Args = Record<string, unknown>

const MAX_QUOTE = 80

const str = (a: Args, key: string): string =>
  typeof a[key] === 'string' ? (a[key] as string).trim() : ''

const num = (a: Args, key: string): number | null =>
  typeof a[key] === 'number' && Number.isFinite(a[key]) ? (a[key] as number) : null

const flag = (a: Args, key: string): boolean | null =>
  typeof a[key] === 'boolean' ? (a[key] as boolean) : null

const size = (a: Args, key: string): number => (Array.isArray(a[key]) ? (a[key] as unknown[]).length : 0)

const tally = (n: number, thing: string) => `${n} ${thing}${n === 1 ? '' : 's'}`

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
  if (kind === 'review') return 'Your week'
  if (kind === 'trigger') return 'Note checks in'
  const words = kind.replaceAll('_', ' ').replace('checkin', 'check-in').trim()
  return words.charAt(0).toUpperCase() + words.slice(1)
}

function dayWord(iso: string): string {
  if (!iso) return 'today'
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

// The day as an adverbial: "today", "on Mon, Oct 3".
function dayPhrase(iso: string): string {
  const word = dayWord(iso)
  return word === 'today' || word === 'tomorrow' ? word : `on ${word}`
}

// When Note will look at the day again, in the words the user set the time in.
function laid(a: Args, key: string): string {
  const at = str(a, key)
  const offset = at.startsWith('+') ? `in ${span(Number(at.replace(/\D/g, '')) || 0)}` : ''
  const when = offset || (at ? `at ${at}` : '')
  return when ? `Set a check-in ${when}` : 'Set a check-in'
}

type Line = string | ((a: Args) => string)

type Receipt = { doing: Line; done: Line; failed: string }

// Every tool the server offers, by domain, in the order the registries list them.
const TABLE: Record<string, Receipt> = {
  memory_query: {
    doing: (a) => {
      const q = str(a, 'query')
      return q ? `Searching memory for ${quoted(q)}` : 'Searching memory'
    },
    done: (a) => {
      const q = str(a, 'query')
      return q ? `Searched memory for ${quoted(q)}` : 'Searched memory'
    },
    failed: "Couldn't search memory",
  },
  memory_read: {
    doing: 'Opening a saved note',
    done: 'Read a saved note',
    failed: "Couldn't open that note",
  },
  memory_write: {
    doing: 'Saving that to memory',
    done: (a) => {
      const summary = str(a, 'summary')
      const tail = summary ? `: ${clip(summary)}` : ''
      if (str(a, 'op') === 'update') return `Updated a saved note${tail}`
      if (str(a, 'op') === 'supersede') return `Replaced an older note${tail}`
      return `Remembered${tail}`
    },
    failed: "Couldn't save that to memory",
  },

  task_create: {
    doing: 'Adding a task',
    done: (a) => {
      const title = str(a, 'title')
      return title ? `Added ${quoted(title)} to your tasks` : 'Added a task'
    },
    failed: "Couldn't add that task",
  },
  task_update: {
    doing: 'Updating a task',
    done: (a) => {
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
      const now = flag(a, 'is_now')
      if (now === true) return 'Moved a task into Now'
      if (now === false) return 'Moved a task back to Later'
      const title = str(a, 'title')
      if (title) return `Renamed a task to ${quoted(title)}`
      if (str(a, 'notes')) return 'Added a note to a task'
      if (str(a, 'description')) return "Filled in a task's details"
      const progress = num(a, 'progress')
      if (progress !== null) return `Set a task to ${progress}% done`
      return 'Updated a task'
    },
    failed: "Couldn't update that task",
  },
  task_split: {
    doing: 'Breaking a task into steps',
    done: (a) => {
      const steps = size(a, 'steps')
      return steps ? `Broke a task into ${tally(steps, 'step')}` : 'Broke a task into steps'
    },
    failed: "Couldn't break that task into steps",
  },
  task_delete: {
    doing: 'Deleting a task',
    done: 'Deleted a task',
    failed: "Couldn't delete that task",
  },
  task_list: {
    doing: 'Looking over your tasks',
    done: (a) => {
      if (flag(a, 'overdue') === true) return 'Looked over what is overdue'
      if (flag(a, 'is_now') === true) return 'Looked over what you are on now'
      const state = str(a, 'state')
      if (state === 'done') return 'Looked over what you have finished'
      if (state === 'dropped') return 'Looked over what you dropped'
      const keyword = str(a, 'keyword')
      if (keyword) return `Looked over your tasks matching ${quoted(keyword)}`
      const due = str(a, 'due_before')
      if (due) return `Looked over what is due by ${dayWord(due)}`
      return 'Looked over your tasks'
    },
    failed: "Couldn't read your tasks",
  },
  task_search: {
    doing: (a) => {
      const q = str(a, 'query')
      return q ? `Looking for ${quoted(q)} in your tasks` : 'Searching your tasks'
    },
    done: (a) => {
      const q = str(a, 'query')
      return q ? `Looked for ${quoted(q)} in your tasks` : 'Searched your tasks'
    },
    failed: "Couldn't search your tasks",
  },
  task_read: {
    doing: 'Opening a task',
    done: 'Read a task',
    failed: "Couldn't open that task",
  },
  task_bulk_update: {
    doing: 'Updating several tasks',
    done: (a) => {
      const n = size(a, 'task_ids')
      const many = tally(n, 'task')
      if (flag(a, 'delete') === true) return `Deleted ${many}`
      const now = flag(a, 'is_now')
      if (now === true) return `Moved ${many} into Now`
      if (now === false) return `Moved ${many} back to Later`
      switch (str(a, 'state')) {
        case 'done':
          return `Marked ${many} done`
        case 'dropped':
          return `Dropped ${many}`
        case 'in_progress':
          return `Started ${many}`
        case 'open':
          return `Reopened ${many}`
      }
      return `Updated ${many}`
    },
    failed: "Couldn't update those tasks",
  },

  note_add: {
    doing: 'Adding a note',
    done: (a) => {
      const text = str(a, 'text')
      return text ? `Noted ${quoted(text)}` : 'Added a note'
    },
    failed: "Couldn't add that note",
  },
  note_update: {
    doing: 'Updating a note',
    done: (a) => {
      const pinned = flag(a, 'pinned')
      if (pinned === true) return 'Pinned a note'
      if (pinned === false) return 'Unpinned a note'
      const text = str(a, 'text')
      return text ? `Changed a note to ${quoted(text)}` : 'Updated a note'
    },
    failed: "Couldn't update that note",
  },
  note_done: {
    doing: 'Checking off a note',
    done: 'Checked off a note',
    failed: "Couldn't check off that note",
  },
  note_list: {
    doing: 'Reading your notes',
    done: 'Read your notes',
    failed: "Couldn't read your notes",
  },

  plan_tasks: {
    doing: 'Laying tasks onto the day',
    done: (a) => {
      const n = size(a, 'task_ids')
      const start = str(a, 'start')
      const from = start ? ` from ${start}` : ''
      return `Laid ${tally(n, 'task')} onto ${dayWord(str(a, 'date'))}${from}`
    },
    failed: "Couldn't lay those tasks onto the day",
  },
  plan_auto: {
    doing: "Filling the day's free time",
    done: (a) => `Filled the free time ${dayPhrase(str(a, 'date'))}`,
    failed: "Couldn't fill that day's free time",
  },
  plan_carry: {
    doing: 'Carrying the rest of the day over',
    done: (a) => `Carried the rest of ${dayWord(str(a, 'date'))} over`,
    failed: "Couldn't carry the rest of the day over",
  },
  plan_list: {
    doing: 'Reading the plan',
    done: (a) => `Looked at ${dayWord(str(a, 'date'))}'s plan`,
    failed: "Couldn't read that plan",
  },

  schedule_slide: {
    doing: 'Moving an event',
    done: (a) => {
      const minutes = num(a, 'minutes')
      if (minutes === null || minutes === 0) return 'Moved an event'
      return `Moved an event ${span(minutes)} ${minutes < 0 ? 'earlier' : 'later'}`
    },
    failed: "Couldn't move that event",
  },
  schedule_snooze: {
    doing: 'Putting an event off',
    done: (a) => {
      const minutes = num(a, 'minutes')
      return minutes === null ? 'Put an event off' : `Put an event off for ${span(minutes)}`
    },
    failed: "Couldn't put that event off",
  },
  schedule_drop: {
    doing: 'Dropping an event',
    done: 'Dropped an event from the day',
    failed: "Couldn't drop that event",
  },
  schedule_reshape: {
    doing: 'Reshaping a block of time',
    done: (a) => {
      const start = str(a, 'start')
      const end = str(a, 'end')
      if (start && end) return `Reshaped a block to ${start}–${end}`
      if (start) return `Moved a block to ${start}`
      if (end) return `Stretched a block to ${end}`
      return 'Reshaped a block'
    },
    failed: "Couldn't reshape that block",
  },
  schedule_insert: {
    doing: 'Adding that to the plan',
    done: (a) => {
      const kind = str(a, 'kind')
      const name = kind ? quoted(eventLabel(kind)) : 'an event'
      const date = str(a, 'date')
      const time = str(a, 'time')
      const when = [date ? dayWord(date) : '', time ? `at ${time}` : ''].filter(Boolean).join(' ')
      return when ? `Added ${name} to ${when}` : `Added ${name} to the plan`
    },
    failed: "Couldn't add that to the plan",
  },

  calendar_list: {
    doing: 'Reading your calendar',
    done: 'Read your calendar',
    failed: "Couldn't read your calendar",
  },
  calendar_add: {
    doing: 'Adding that to your calendar',
    done: (a) => {
      const title = str(a, 'title')
      return title ? `Added ${quoted(title)} to your calendar` : 'Added a calendar entry'
    },
    failed: "Couldn't add that to your calendar",
  },
  calendar_update: {
    doing: 'Updating a calendar entry',
    done: (a) => {
      const title = str(a, 'title')
      return title ? `Renamed a calendar entry to ${quoted(title)}` : 'Updated a calendar entry'
    },
    failed: "Couldn't update that calendar entry",
  },
  calendar_remove: {
    doing: 'Removing a calendar entry',
    done: 'Removed a calendar entry',
    failed: "Couldn't remove that calendar entry",
  },
  calendar_skip: {
    doing: 'Skipping a day of a calendar entry',
    done: (a) => `Skipped a calendar entry ${dayPhrase(str(a, 'date'))}`,
    failed: "Couldn't skip that day",
  },

  trigger_set: {
    doing: 'Setting a check-in',
    done: (a) => laid(a, 'at'),
    failed: "Couldn't set that check-in",
  },
  wait_until: {
    doing: 'Setting a check-in',
    done: (a) => laid(a, 'at'),
    failed: "Couldn't set that check-in",
  },
  wait_for: {
    doing: 'Setting a check-in',
    done: (a) => laid(a, 'until'),
    failed: "Couldn't set that check-in",
  },
  trigger_budget: {
    doing: "Raising today's check-in budget",
    done: (a) => {
      const extra = num(a, 'extra')
      return extra === null
        ? "Raised today's check-in budget"
        : `Raised today's check-in budget by ${extra}`
    },
    failed: "Couldn't raise today's check-in budget",
  },

  context_edit: {
    doing: 'Updating your background notes',
    done: (a) => {
      const append = str(a, 'append')
      return append
        ? `Added ${quoted(append)} to your background notes`
        : 'Updated your background notes'
    },
    failed: "Couldn't update your background notes",
  },

  notify_send: {
    doing: 'Sending you a nudge',
    done: (a) => {
      const text = str(a, 'text')
      return text ? `Sent you a nudge: ${clip(text)}` : 'Sent you a nudge'
    },
    failed: "Couldn't send that nudge",
  },

  web_search: {
    doing: (a) => {
      const q = str(a, 'query')
      return q ? `Searching the web for ${quoted(q)}` : 'Searching the web'
    },
    done: (a) => {
      const q = str(a, 'query')
      return q ? `Searched the web for ${quoted(q)}` : 'Searched the web'
    },
    failed: "Couldn't search the web",
  },

  say: {
    doing: 'Writing back',
    done: (a) => {
      const text = str(a, 'text')
      return text ? `Said: ${clip(text)}` : 'Said something'
    },
    failed: "Couldn't say that",
  },
  stay_quiet: {
    doing: 'Deciding whether to say anything',
    done: 'Stayed quiet',
    failed: "Couldn't close that check-in",
  },
  summary_write: {
    doing: 'Summing up this conversation',
    done: (a) => {
      const summary = str(a, 'summary')
      return summary ? `Summed up this conversation: ${clip(summary)}` : 'Summed up this conversation'
    },
    failed: "Couldn't sum up this conversation",
  },
  harvest_done: {
    doing: 'Finishing the harvest',
    done: (a) => {
      const written = num(a, 'written') ?? 0
      const note = str(a, 'note')
      return `Wrote ${tally(written, 'fact')} to memory${note ? `: ${clip(note)}` : ''}`
    },
    failed: "Couldn't finish the harvest",
  },
  review_write: {
    doing: 'Writing your week',
    done: 'Wrote your week',
    failed: "Couldn't write your week",
  },
  task_brief: {
    doing: 'Briefing an assignment',
    done: (a) => {
      if (flag(a, 'homework') === false) {
        const reason = str(a, 'reason')
        return reason ? `Set an import aside: ${clip(reason)}` : 'Set an import aside'
      }
      return 'Briefed an assignment'
    },
    failed: "Couldn't brief that assignment",
  },
  inbox_decide: {
    doing: 'Deciding on an inbox item',
    done: (a) => {
      switch (str(a, 'outcome')) {
        case 'remembered':
          return 'Remembered what an item was worth'
        case 'nothing':
          return 'Let an item go'
        case 'task':
          return 'Turned an item into a task'
      }
      return 'Decided on an item'
    },
    failed: "Couldn't decide on that item",
  },
  nightly_notes_write: {
    doing: 'Leaving tomorrow a brief',
    done: 'Left tomorrow a brief',
    failed: "Couldn't leave tomorrow a brief",
  },

  batch: {
    doing: 'Doing several things at once',
    done: (a) => `Did ${tally(size(a, 'calls'), 'thing')} at once`,
    failed: "Couldn't do those at once",
  },
}

function parse(raw: string): Args {
  try {
    const value: unknown = JSON.parse(raw)
    return typeof value === 'object' && value !== null ? (value as Args) : {}
  } catch {
    return {}
  }
}

const say = (line: Line, a: Args) => (typeof line === 'string' ? line : line(a))

// The sentence for a call still in flight.
export function doing(name: string, args: string): string {
  const entry = TABLE[name]
  return entry ? say(entry.doing, parse(args)) : `Using ${name}`
}

// The sentence for one tool call; an unrecognised tool still gets a sentence, never a raw block.
export function receipt(name: string, args: string, isError: boolean): string {
  const entry = TABLE[name]
  if (isError) return entry?.failed ?? `Couldn't use ${name}`
  return entry ? say(entry.done, parse(args)) : `Used ${name}`
}

// A batch's inner calls, paired with the server's `{ results: [{ tool, ok, result | error }] }`.
export function batchReceipts(
  args: string,
  result: string,
): { name: string; args: string; result: string; isError: boolean }[] {
  const calls = parse(args).calls
  if (!Array.isArray(calls)) return []
  const outcomes = parse(result).results
  const results = Array.isArray(outcomes) ? outcomes : []
  return calls.map((raw, i) => {
    const call = (typeof raw === 'object' && raw !== null ? raw : {}) as Args
    const outcome = (typeof results[i] === 'object' && results[i] !== null
      ? results[i]
      : {}) as Args
    const isError = outcome.ok === false
    const payload = isError ? outcome.error : outcome.result
    return {
      name: typeof call.tool === 'string' ? call.tool : '',
      args: JSON.stringify(call.args ?? {}),
      result: payload === undefined ? '' : JSON.stringify(payload),
      isError,
    }
  })
}
