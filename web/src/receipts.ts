import { t, type Key } from './i18n'
import { weekdayDay } from './i18n/format'

type Args = Record<string, unknown>

const MAX_QUOTE = 80

const str = (a: Args, key: string): string =>
  typeof a[key] === 'string' ? (a[key] as string).trim() : ''

const num = (a: Args, key: string): number | null =>
  typeof a[key] === 'number' && Number.isFinite(a[key]) ? (a[key] as number) : null

const flag = (a: Args, key: string): boolean | null =>
  typeof a[key] === 'boolean' ? (a[key] as boolean) : null

const size = (a: Args, key: string): number => (Array.isArray(a[key]) ? (a[key] as unknown[]).length : 0)

function clip(text: string): string {
  const flat = text.replace(/\s+/g, ' ').trim()
  return flat.length > MAX_QUOTE ? `${flat.slice(0, MAX_QUOTE - 1)}…` : flat
}

function span(minutes: number): string {
  const m = Math.abs(minutes)
  if (m < 60 || m % 60 !== 0) return t('receipt.minutes', { n: m })
  return t('receipt.hours', { n: m / 60 })
}

const KIND_LABEL: Record<string, Key> = {
  debrief: 'receipt.kind.debrief',
  review: 'receipt.kind.review',
  trigger: 'receipt.kind.trigger',
}

// Any other kind is the user's own routine name, shown as written.
export function eventLabel(kind: string): string {
  if (kind in KIND_LABEL) return t(KIND_LABEL[kind])
  const words = kind.replaceAll('_', ' ').replace('checkin', 'check-in').trim()
  return words.charAt(0).toUpperCase() + words.slice(1)
}

// "today", "tomorrow" (`near`), or the date itself.
function dayOf(iso: string): { text: string; near: boolean } {
  if (!iso) return { text: t('receipt.today'), near: true }
  const at = new Date(`${iso}T00:00`)
  if (Number.isNaN(at.getTime())) return { text: iso, near: false }
  const days = Math.round((at.setHours(0, 0, 0, 0) - new Date().setHours(0, 0, 0, 0)) / 86_400_000)
  if (days === 0) return { text: t('receipt.today'), near: true }
  if (days === 1) return { text: t('receipt.tomorrow'), near: true }
  return { text: weekdayDay(new Date(`${iso}T00:00`)), near: false }
}

const dayWord = (iso: string) => dayOf(iso).text

// The day as an adverbial: "today", "on Mon, Oct 3".
function dayPhrase(iso: string): string {
  const day = dayOf(iso)
  return day.near ? day.text : t('receipt.onDay', { day: day.text })
}

// When Note will look at the day again, in the words the user set the time in.
function laid(a: Args, key: string): string {
  const at = str(a, key)
  if (at.startsWith('+')) return t('receipt.checkIn.in', { span: span(Number(at.replace(/\D/g, '')) || 0) })
  return at ? t('receipt.checkIn.at', { time: at }) : t('receipt.checkIn.set')
}

const INSERTED: Record<string, Key> = {
  'named.dayTime': 'receipt.scheduleInsert.namedDayTime',
  'named.day': 'receipt.scheduleInsert.namedDay',
  'named.time': 'receipt.scheduleInsert.namedTime',
  'named.plan': 'receipt.scheduleInsert.namedPlan',
  'event.dayTime': 'receipt.scheduleInsert.eventDayTime',
  'event.day': 'receipt.scheduleInsert.eventDay',
  'event.time': 'receipt.scheduleInsert.eventTime',
  'event.plan': 'receipt.scheduleInsert.eventPlan',
}

type Line = Key | ((a: Args) => string)

type Receipt = { doing: Line; done: Line; failed: Key }

// Every tool the server offers, by domain, in the order the registries list them.
const TABLE: Record<string, Receipt> = {
  memory_query: {
    doing: (a) => {
      const q = str(a, 'query')
      return q ? t('receipt.memoryQuery.doingFor', { query: clip(q) }) : t('receipt.memoryQuery.doing')
    },
    done: (a) => {
      const q = str(a, 'query')
      return q ? t('receipt.memoryQuery.doneFor', { query: clip(q) }) : t('receipt.memoryQuery.done')
    },
    failed: 'receipt.memoryQuery.failed',
  },
  memory_read: {
    doing: 'receipt.memoryRead.doing',
    done: 'receipt.memoryRead.done',
    failed: 'receipt.memoryRead.failed',
  },
  memory_write: {
    doing: 'receipt.memoryWrite.doing',
    done: (a) => {
      const summary = clip(str(a, 'summary'))
      const op = str(a, 'op')
      if (op === 'update')
        return summary ? t('receipt.memoryWrite.updatedWith', { summary }) : t('receipt.memoryWrite.updated')
      if (op === 'supersede')
        return summary ? t('receipt.memoryWrite.replacedWith', { summary }) : t('receipt.memoryWrite.replaced')
      return summary ? t('receipt.memoryWrite.rememberedWith', { summary }) : t('receipt.memoryWrite.remembered')
    },
    failed: 'receipt.memoryWrite.failed',
  },

  task_create: {
    doing: 'receipt.taskCreate.doing',
    done: (a) => {
      const title = str(a, 'title')
      return title ? t('receipt.taskCreate.doneNamed', { title: clip(title) }) : t('receipt.taskCreate.done')
    },
    failed: 'receipt.taskCreate.failed',
  },
  task_update: {
    doing: 'receipt.taskUpdate.doing',
    done: (a) => {
      switch (str(a, 'state')) {
        case 'done':
          return t('receipt.taskUpdate.done')
        case 'dropped':
          return t('receipt.taskUpdate.dropped')
        case 'in_progress':
          return t('receipt.taskUpdate.started')
        case 'open':
          return t('receipt.taskUpdate.reopened')
      }
      const now = flag(a, 'is_now')
      if (now === true) return t('receipt.taskUpdate.toNow')
      if (now === false) return t('receipt.taskUpdate.toLater')
      const title = str(a, 'title')
      if (title) return t('receipt.taskUpdate.renamed', { title: clip(title) })
      if (str(a, 'notes')) return t('receipt.taskUpdate.noted')
      if (str(a, 'description')) return t('receipt.taskUpdate.described')
      const progress = num(a, 'progress')
      if (progress !== null) return t('receipt.taskUpdate.progress', { n: progress })
      return t('receipt.taskUpdate.updated')
    },
    failed: 'receipt.taskUpdate.failed',
  },
  task_split: {
    doing: 'receipt.taskSplit.doing',
    done: (a) => {
      const count = size(a, 'steps')
      return count ? t('receipt.taskSplit.doneCount', { count }) : t('receipt.taskSplit.done')
    },
    failed: 'receipt.taskSplit.failed',
  },
  task_delete: {
    doing: 'receipt.taskDelete.doing',
    done: 'receipt.taskDelete.done',
    failed: 'receipt.taskDelete.failed',
  },
  task_list: {
    doing: 'receipt.taskList.doing',
    done: (a) => {
      if (flag(a, 'overdue') === true) return t('receipt.taskList.overdue')
      if (flag(a, 'is_now') === true) return t('receipt.taskList.now')
      const state = str(a, 'state')
      if (state === 'done') return t('receipt.taskList.finished')
      if (state === 'dropped') return t('receipt.taskList.dropped')
      const keyword = str(a, 'keyword')
      if (keyword) return t('receipt.taskList.matching', { keyword: clip(keyword) })
      const due = str(a, 'due_before')
      if (due) return t('receipt.taskList.dueBy', { day: dayWord(due) })
      return t('receipt.taskList.done')
    },
    failed: 'receipt.taskList.failed',
  },
  task_search: {
    doing: (a) => {
      const q = str(a, 'query')
      return q ? t('receipt.taskSearch.doingFor', { query: clip(q) }) : t('receipt.taskSearch.doing')
    },
    done: (a) => {
      const q = str(a, 'query')
      return q ? t('receipt.taskSearch.doneFor', { query: clip(q) }) : t('receipt.taskSearch.done')
    },
    failed: 'receipt.taskSearch.failed',
  },
  task_read: {
    doing: 'receipt.taskRead.doing',
    done: 'receipt.taskRead.done',
    failed: 'receipt.taskRead.failed',
  },
  task_bulk_update: {
    doing: 'receipt.taskBulk.doing',
    done: (a) => {
      const count = size(a, 'task_ids')
      if (flag(a, 'delete') === true) return t('receipt.taskBulk.deleted', { count })
      const now = flag(a, 'is_now')
      if (now === true) return t('receipt.taskBulk.toNow', { count })
      if (now === false) return t('receipt.taskBulk.toLater', { count })
      switch (str(a, 'state')) {
        case 'done':
          return t('receipt.taskBulk.done', { count })
        case 'dropped':
          return t('receipt.taskBulk.dropped', { count })
        case 'in_progress':
          return t('receipt.taskBulk.started', { count })
        case 'open':
          return t('receipt.taskBulk.reopened', { count })
      }
      return t('receipt.taskBulk.updated', { count })
    },
    failed: 'receipt.taskBulk.failed',
  },

  note_add: {
    doing: 'receipt.noteAdd.doing',
    done: (a) => {
      const text = str(a, 'text')
      return text ? t('receipt.noteAdd.doneText', { text: clip(text) }) : t('receipt.noteAdd.done')
    },
    failed: 'receipt.noteAdd.failed',
  },
  note_update: {
    doing: 'receipt.noteUpdate.doing',
    done: (a) => {
      const pinned = flag(a, 'pinned')
      if (pinned === true) return t('receipt.noteUpdate.pinned')
      if (pinned === false) return t('receipt.noteUpdate.unpinned')
      const text = str(a, 'text')
      return text ? t('receipt.noteUpdate.changed', { text: clip(text) }) : t('receipt.noteUpdate.done')
    },
    failed: 'receipt.noteUpdate.failed',
  },
  note_done: {
    doing: 'receipt.noteDone.doing',
    done: 'receipt.noteDone.done',
    failed: 'receipt.noteDone.failed',
  },
  note_list: {
    doing: 'receipt.noteList.doing',
    done: 'receipt.noteList.done',
    failed: 'receipt.noteList.failed',
  },

  plan_tasks: {
    doing: 'receipt.planTasks.doing',
    done: (a) => {
      const vars = { count: size(a, 'task_ids'), day: dayWord(str(a, 'date')) }
      const start = str(a, 'start')
      return start ? t('receipt.planTasks.doneFrom', { ...vars, start }) : t('receipt.planTasks.done', vars)
    },
    failed: 'receipt.planTasks.failed',
  },
  plan_auto: {
    doing: 'receipt.planAuto.doing',
    done: (a) => t('receipt.planAuto.done', { day: dayPhrase(str(a, 'date')) }),
    failed: 'receipt.planAuto.failed',
  },
  plan_carry: {
    doing: 'receipt.planCarry.doing',
    done: (a) => t('receipt.planCarry.done', { day: dayWord(str(a, 'date')) }),
    failed: 'receipt.planCarry.failed',
  },
  plan_list: {
    doing: 'receipt.planList.doing',
    done: (a) => t('receipt.planList.done', { day: dayWord(str(a, 'date')) }),
    failed: 'receipt.planList.failed',
  },

  schedule_slide: {
    doing: 'receipt.scheduleSlide.doing',
    done: (a) => {
      const minutes = num(a, 'minutes')
      if (minutes === null || minutes === 0) return t('receipt.scheduleSlide.done')
      return t(minutes < 0 ? 'receipt.scheduleSlide.earlier' : 'receipt.scheduleSlide.later', {
        span: span(minutes),
      })
    },
    failed: 'receipt.scheduleSlide.failed',
  },
  schedule_snooze: {
    doing: 'receipt.scheduleSnooze.doing',
    done: (a) => {
      const minutes = num(a, 'minutes')
      return minutes === null
        ? t('receipt.scheduleSnooze.done')
        : t('receipt.scheduleSnooze.doneFor', { span: span(minutes) })
    },
    failed: 'receipt.scheduleSnooze.failed',
  },
  schedule_drop: {
    doing: 'receipt.scheduleDrop.doing',
    done: 'receipt.scheduleDrop.done',
    failed: 'receipt.scheduleDrop.failed',
  },
  schedule_reshape: {
    doing: 'receipt.scheduleReshape.doing',
    done: (a) => {
      const start = str(a, 'start')
      const end = str(a, 'end')
      if (start && end) return t('receipt.scheduleReshape.span', { start, end })
      if (start) return t('receipt.scheduleReshape.moved', { start })
      if (end) return t('receipt.scheduleReshape.stretched', { end })
      return t('receipt.scheduleReshape.done')
    },
    failed: 'receipt.scheduleReshape.failed',
  },
  schedule_insert: {
    doing: 'receipt.scheduleInsert.doing',
    done: (a) => {
      const kind = str(a, 'kind')
      const date = str(a, 'date')
      const time = str(a, 'time')
      const when = date && time ? 'dayTime' : date ? 'day' : time ? 'time' : 'plan'
      return t(INSERTED[`${kind ? 'named' : 'event'}.${when}`], {
        name: kind ? eventLabel(kind) : '',
        day: date ? dayWord(date) : '',
        time,
      })
    },
    failed: 'receipt.scheduleInsert.failed',
  },

  calendar_list: {
    doing: 'receipt.calendarList.doing',
    done: 'receipt.calendarList.done',
    failed: 'receipt.calendarList.failed',
  },
  calendar_add: {
    doing: 'receipt.calendarAdd.doing',
    done: (a) => {
      const title = str(a, 'title')
      return title ? t('receipt.calendarAdd.doneNamed', { title: clip(title) }) : t('receipt.calendarAdd.done')
    },
    failed: 'receipt.calendarAdd.failed',
  },
  calendar_update: {
    doing: 'receipt.calendarUpdate.doing',
    done: (a) => {
      const title = str(a, 'title')
      return title
        ? t('receipt.calendarUpdate.renamed', { title: clip(title) })
        : t('receipt.calendarUpdate.done')
    },
    failed: 'receipt.calendarUpdate.failed',
  },
  calendar_remove: {
    doing: 'receipt.calendarRemove.doing',
    done: 'receipt.calendarRemove.done',
    failed: 'receipt.calendarRemove.failed',
  },
  calendar_skip: {
    doing: 'receipt.calendarSkip.doing',
    done: (a) => t('receipt.calendarSkip.done', { day: dayPhrase(str(a, 'date')) }),
    failed: 'receipt.calendarSkip.failed',
  },

  trigger_set: {
    doing: 'receipt.checkIn.doing',
    done: (a) => laid(a, 'at'),
    failed: 'receipt.checkIn.failed',
  },
  wait_until: {
    doing: 'receipt.checkIn.doing',
    done: (a) => laid(a, 'at'),
    failed: 'receipt.checkIn.failed',
  },
  wait_for: {
    doing: 'receipt.checkIn.doing',
    done: (a) => laid(a, 'until'),
    failed: 'receipt.checkIn.failed',
  },
  trigger_budget: {
    doing: 'receipt.triggerBudget.doing',
    done: (a) => {
      const extra = num(a, 'extra')
      return extra === null ? t('receipt.triggerBudget.done') : t('receipt.triggerBudget.doneBy', { n: extra })
    },
    failed: 'receipt.triggerBudget.failed',
  },

  context_edit: {
    doing: 'receipt.contextEdit.doing',
    done: (a) => {
      const append = str(a, 'append')
      return append ? t('receipt.contextEdit.added', { text: clip(append) }) : t('receipt.contextEdit.done')
    },
    failed: 'receipt.contextEdit.failed',
  },

  notify_send: {
    doing: 'receipt.notifySend.doing',
    done: (a) => {
      const text = str(a, 'text')
      return text ? t('receipt.notifySend.doneText', { text: clip(text) }) : t('receipt.notifySend.done')
    },
    failed: 'receipt.notifySend.failed',
  },

  web_search: {
    doing: (a) => {
      const q = str(a, 'query')
      return q ? t('receipt.webSearch.doingFor', { query: clip(q) }) : t('receipt.webSearch.doing')
    },
    done: (a) => {
      const q = str(a, 'query')
      return q ? t('receipt.webSearch.doneFor', { query: clip(q) }) : t('receipt.webSearch.done')
    },
    failed: 'receipt.webSearch.failed',
  },

  say: {
    doing: 'receipt.say.doing',
    done: (a) => {
      const text = str(a, 'text')
      return text ? t('receipt.say.doneText', { text: clip(text) }) : t('receipt.say.done')
    },
    failed: 'receipt.say.failed',
  },
  stay_quiet: {
    doing: 'receipt.stayQuiet.doing',
    done: 'receipt.stayQuiet.done',
    failed: 'receipt.stayQuiet.failed',
  },
  summary_write: {
    doing: 'receipt.summaryWrite.doing',
    done: (a) => {
      const summary = str(a, 'summary')
      return summary
        ? t('receipt.summaryWrite.doneText', { summary: clip(summary) })
        : t('receipt.summaryWrite.done')
    },
    failed: 'receipt.summaryWrite.failed',
  },
  harvest_done: {
    doing: 'receipt.harvest.doing',
    done: (a) => {
      const count = num(a, 'written') ?? 0
      const note = str(a, 'note')
      return note ? t('receipt.harvest.doneNote', { count, note: clip(note) }) : t('receipt.harvest.done', { count })
    },
    failed: 'receipt.harvest.failed',
  },
  review_write: {
    doing: 'receipt.reviewWrite.doing',
    done: 'receipt.reviewWrite.done',
    failed: 'receipt.reviewWrite.failed',
  },
  task_brief: {
    doing: 'receipt.taskBrief.doing',
    done: (a) => {
      if (flag(a, 'homework') === false) {
        const reason = str(a, 'reason')
        return reason ? t('receipt.taskBrief.asideFor', { reason: clip(reason) }) : t('receipt.taskBrief.aside')
      }
      return t('receipt.taskBrief.done')
    },
    failed: 'receipt.taskBrief.failed',
  },
  inbox_decide: {
    doing: 'receipt.inboxDecide.doing',
    done: (a) => {
      switch (str(a, 'outcome')) {
        case 'remembered':
          return t('receipt.inboxDecide.remembered')
        case 'nothing':
          return t('receipt.inboxDecide.nothing')
        case 'task':
          return t('receipt.inboxDecide.task')
      }
      return t('receipt.inboxDecide.done')
    },
    failed: 'receipt.inboxDecide.failed',
  },
  nightly_notes_write: {
    doing: 'receipt.nightly.doing',
    done: 'receipt.nightly.done',
    failed: 'receipt.nightly.failed',
  },

  batch: {
    doing: 'receipt.batch.doing',
    done: (a) => t('receipt.batch.done', { count: size(a, 'calls') }),
    failed: 'receipt.batch.failed',
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

const say = (line: Line, a: Args) => (typeof line === 'string' ? t(line) : line(a))

// The sentence for a call still in flight.
export function doing(name: string, args: string): string {
  const entry = TABLE[name]
  return entry ? say(entry.doing, parse(args)) : t('receipt.using', { name })
}

// The sentence for one tool call; an unrecognised tool still gets a sentence, never a raw block.
export function receipt(name: string, args: string, isError: boolean): string {
  const entry = TABLE[name]
  if (isError) return entry ? t(entry.failed) : t('receipt.useFailed', { name })
  return entry ? say(entry.done, parse(args)) : t('receipt.used', { name })
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
