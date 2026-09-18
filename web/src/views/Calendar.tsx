import gsap from 'gsap'
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
} from 'react'
import { createPortal } from 'react-dom'
import { api } from '../api'
import type { ViewProps } from '../app'
import { minutesOf } from '../dayline'
import { useEscape } from '../escape'
import { makeHold } from '../held'
import { reducedMotion } from '../motion'
import type { CalendarEntry, CalendarKind, DayView, PlanEvent } from '../types'
import '../styles/calendar.css'

const START = 6 * 60
const END = 24 * 60
const HOURS = [6, 9, 12, 15, 18, 21, 24]
const LETTERS = ['M', 'T', 'W', 'T', 'F', 'S', 'S']
const DAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun']
const MONTHS = [
  'January',
  'February',
  'March',
  'April',
  'May',
  'June',
  'July',
  'August',
  'September',
  'October',
  'November',
  'December',
]
const KINDS: CalendarKind[] = ['fixed', 'busy', 'note', 'free']
const KIND_LABEL: Record<CalendarKind, string> = {
  fixed: 'Fixed',
  busy: 'Busy',
  note: 'Note',
  free: 'Free time',
}

// Removal has no server-side reversal, so the request waits out the undo window.
const deleteHold = makeHold<number>()

const pad = (n: number) => String(n).padStart(2, '0')
const pct = (m: number) => `${((Math.min(END, Math.max(START, m)) - START) / (END - START)) * 100}%`

const isoOf = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`

function dateOf(iso: string): Date {
  const [y, m, d] = iso.split('-').map(Number)
  return new Date(y, m - 1, d)
}

function shift(iso: string, days: number): string {
  const d = dateOf(iso)
  d.setDate(d.getDate() + days)
  return isoOf(d)
}

// Monday = 0, the order the server's day mask counts in.
const weekdayOf = (iso: string) => (dateOf(iso).getDay() + 6) % 7
const mondayOf = (iso: string) => shift(iso, -weekdayOf(iso))

type Occurrence = {
  entry: CalendarEntry
  start: number
  end: number
  once: boolean
  skipped: boolean
}

function occurrencesOn(entries: CalendarEntry[], date: string): Occurrence[] {
  const bit = 1 << weekdayOf(date)
  return entries
    .filter((e) =>
      e.days
        ? (e.days & bit) !== 0 &&
          (!e.from_date || date >= e.from_date) &&
          (!e.until_date || date <= e.until_date)
        : e.on_date === date,
    )
    .map((e) => ({
      entry: e,
      start: minutesOf(e.start_time),
      end: minutesOf(e.end_time),
      once: e.days === 0,
      skipped: e.exceptions.includes(date),
    }))
    .sort((a, b) => a.start - b.start || a.end - b.end)
}

const TIME = /^([01]\d|2[0-3]):[0-5]\d$/

// What the field holds becomes the zero-padded HH:MM the server takes, when it can.
function asTime(raw: string): string {
  const m = /^(\d{1,2})[:.]?(\d{2})$/.exec(raw.trim())
  if (!m) return raw.trim()
  const [h, min] = [Number(m[1]), Number(m[2])]
  return h < 24 && min < 60 ? `${pad(h)}:${pad(min)}` : raw.trim()
}

const spanOf = (o: Occurrence) =>
  o.entry.kind === 'note' ? o.entry.start_time : `${o.entry.start_time} – ${o.entry.end_time}`

const bandClass = (o: Occurrence, base: string) =>
  [base, o.entry.kind, o.once ? 'once' : '', o.skipped ? 'skipped' : ''].filter(Boolean).join(' ')

function useMedia(query: string): boolean {
  const [matches, setMatches] = useState(() => window.matchMedia(query).matches)
  useEffect(() => {
    const mq = window.matchMedia(query)
    const on = () => setMatches(mq.matches)
    mq.addEventListener('change', on)
    return () => mq.removeEventListener('change', on)
  }, [query])
  return matches
}

export function CalendarSection({
  notify,
  refresh,
  onChanged,
  day,
  onStartBlock,
}: Pick<ViewProps, 'notify' | 'refresh' | 'onChanged'> & {
  // Today, as Home read it: the quiet window and the free time both come from there.
  day: DayView | null
  onStartBlock: (event: PlanEvent) => void
}) {
  const [entries, setEntries] = useState<CalendarEntry[] | null>(null)
  const [today, setToday] = useState(() => isoOf(new Date()))
  const [selected, setSelected] = useState(today)
  const [grid, setGrid] = useState(false)
  const [sheet, setSheet] = useState<{ entry: CalendarEntry | null; date: string } | null>(null)
  const [blocks, setBlocks] = useState<Record<string, PlanEvent[]>>({})
  const [block, setBlock] = useState<PlanEvent | null>(null)
  const [filling, setFilling] = useState(false)
  const [, tick] = useState(0)
  const wide = useMedia('(min-width: 768px)')
  const tips = useTips()
  const quietUntil = day?.date === today ? day.quiet_now : null

  const load = useCallback(() => {
    setToday(isoOf(new Date()))
    api
      .calendar()
      .then((r) => setEntries(r.entries))
      .catch(() => notify("Couldn't load the calendar. Try again."))
  }, [notify])
  useEffect(load, [load, refresh])

  useEffect(() => {
    const id = setInterval(() => tick((n) => n + 1), 30_000)
    return () => clearInterval(id)
  }, [])

  const visible = entries?.filter((e) => e.id !== deleteHold.held()) ?? []
  const monday = mondayOf(selected)
  const week = Array.from({ length: 7 }, (_, i) => shift(monday, i))
  const now = new Date().getHours() * 60 + new Date().getMinutes()

  // The task blocks the week already holds; a day with no plan simply has none.
  useEffect(() => {
    let stale = false
    api
      .planRange(week[0], week[6])
      .then((r) => {
        if (stale) return
        const kept: Record<string, PlanEvent[]> = {}
        for (const [date, events] of Object.entries(r.days)) {
          const held = events.filter((e) => e.task && e.status !== 'dropped')
          if (held.length) kept[date] = held
        }
        setBlocks(kept)
      })
      .catch(() => {
        if (!stale) setBlocks({})
      })
    return () => {
      stale = true
    }
  }, [week[0], week[6], refresh])

  const freeToday = (day?.free.length ?? 0) > 0 && selected === today
  const fill = () => {
    if (filling) return
    setFilling(true)
    api
      .allocate(selected)
      .then((out) => {
        notify(out.placed.length ? `${out.placed.length} laid into your free time` : 'Nothing to lay in')
        onChanged()
      })
      .catch(() => notify("Couldn't fill that day. Try again."))
      .finally(() => setFilling(false))
  }

  const settle = (run: Promise<unknown>, failure: string) => {
    setBlock(null)
    run.then(onChanged).catch(() => notify(failure))
  }

  const replace = (entry: CalendarEntry) =>
    setEntries((list) => (list ?? []).map((e) => (e.id === entry.id ? entry : e)))

  const save = (entry: CalendarEntry | null, draft: Draft) => {
    const fields = {
      title: draft.title.trim(),
      kind: draft.kind,
      quiet: draft.quiet,
      start_time: draft.start,
      end_time: draft.end,
      days: draft.days,
      on_date: draft.days ? '' : draft.date,
    }
    if (entry) {
      replace({ ...entry, ...fields, on_date: draft.days ? null : draft.date })
      api
        .patchCalendarEntry(entry.id, fields)
        .then((saved) => {
          replace(saved)
          onChanged()
        })
        .catch(() => {
          notify("Couldn't save that entry. Try again.")
          load()
        })
      return
    }
    const pending: CalendarEntry = {
      id: -Date.now(),
      ...fields,
      on_date: draft.days ? null : draft.date,
      day_names: [],
      from_date: null,
      until_date: null,
      created_at: '',
      updated_at: '',
      exceptions: [],
    }
    setEntries((list) => [...(list ?? []), pending])
    api
      .addCalendarEntry(fields)
      .then((saved) => {
        setEntries((list) => (list ?? []).map((e) => (e.id === pending.id ? saved : e)))
        onChanged()
      })
      .catch(() => {
        setEntries((list) => (list ?? []).filter((e) => e.id !== pending.id))
        notify("Couldn't add that entry. Try again.")
      })
  }

  const remove = (entry: CalendarEntry) => {
    const settled = () => {
      load()
      onChanged()
    }
    deleteHold.start(entry.id, () => {
      api.deleteCalendarEntry(entry.id).then(settled, settled)
    })
    tick((n) => n + 1)
    notify(`${entry.title} — removed`, {
      label: 'Undo',
      run: () => {
        if (deleteHold.cancel(entry.id)) tick((n) => n + 1)
      },
    })
  }

  const setSkipped = (entry: CalendarEntry, date: string, skipped: boolean) => {
    replace({
      ...entry,
      exceptions: skipped
        ? [...entry.exceptions, date].sort()
        : entry.exceptions.filter((d) => d !== date),
    })
    const call = skipped ? api.skipCalendarDate : api.unskipCalendarDate
    call(entry.id, date)
      .then(onChanged)
      .catch(() => {
        notify("Couldn't change that day. Try again.")
        load()
      })
  }

  const skip = (entry: CalendarEntry, date: string) => {
    setSkipped(entry, date, true)
    notify(`${entry.title} — skipped ${DAYS[weekdayOf(date)]}`, {
      label: 'Undo',
      run: () => setSkipped(entry, date, false),
    })
  }

  const month = MONTHS[dateOf(selected).getMonth()]
  const range = `${dateOf(week[0]).getDate()} – ${dateOf(week[6]).getDate()}`
  const showGrid = wide || grid

  return (
    <section
      className={`cal${wide ? ' desktop' : ''}`}
      aria-label="Calendar"
      onMouseOver={tips.over}
      onMouseOut={tips.out}
      onClick={tips.click}
    >
      <div className="cal-head">
        {wide ? (
          <span className="cal-month">
            {month} <span className="cal-range tnum">{range}</span>
          </span>
        ) : (
          <button
            className={`cal-month${grid ? ' open' : ''}`}
            aria-expanded={grid}
            onClick={() => setGrid((v) => !v)}
          >
            {month}
            {grid && <span className="cal-range tnum">{range}</span>}
            <ChevronRight />
          </button>
        )}
        {freeToday && (
          <button className="quiet cal-fill" disabled={filling} onClick={fill}>
            Fill my free time
          </button>
        )}
        <button
          className="btn-round"
          aria-label="Add an entry"
          onClick={() => setSheet({ entry: null, date: selected })}
        >
          <Plus />
        </button>
      </div>
      {showGrid ? (
        <WeekGrid
          week={week}
          today={today}
          now={now}
          entries={visible}
          blocks={blocks}
          quietUntil={quietUntil}
          onPick={(entry, date) => setSheet({ entry, date })}
          onBlock={setBlock}
        />
      ) : (
        <>
          <WeekStrip
            week={week}
            today={today}
            selected={selected}
            entries={visible}
            onSelect={setSelected}
          />
          <div className="cal-line">
            <DayBands
              date={selected}
              today={today}
              now={now}
              entries={visible}
              blocks={blocks[selected] ?? []}
              quietUntil={quietUntil}
              onBlock={setBlock}
            />
          </div>
          <DayList
            date={selected}
            entries={visible}
            onPick={(entry) => setSheet({ entry, date: selected })}
          />
        </>
      )}
      {tips.node}
      {block && (
        <BlockMenu
          event={block}
          onStart={() => {
            const event = block
            setBlock(null)
            onStartBlock(event)
          }}
          onDone={() =>
            settle(
              api.eventAction(block.id, 'done').then(() =>
                block.task ? api.patchTask(block.task.id, { state: 'done' }) : undefined,
              ),
              "Couldn't mark that done. Try again.",
            )
          }
          onDrop={() => settle(api.eventAction(block.id, 'drop'), "Couldn't drop that. Try again.")}
          onMove={() => settle(api.moveTomorrow(block.id), "Couldn't move that. Try again.")}
          onClose={() => setBlock(null)}
        />
      )}
      {sheet && (
        <EntrySheet
          key={sheet.entry?.id ?? 'new'}
          entry={sheet.entry}
          date={sheet.date}
          onClose={() => setSheet(null)}
          onSave={(draft) => save(sheet.entry, draft)}
          onDelete={() => sheet.entry && remove(sheet.entry)}
          onSkip={() => sheet.entry && skip(sheet.entry, sheet.date)}
        />
      )}
    </section>
  )
}

function WeekStrip({
  week,
  today,
  selected,
  entries,
  onSelect,
}: {
  week: string[]
  today: string
  selected: string
  entries: CalendarEntry[]
  onSelect: (date: string) => void
}) {
  return (
    <div className="week-strip">
      {week.map((date, i) => (
        <button
          key={date}
          className={`ws-day${date === today ? ' is-today' : date < today ? ' past' : ''}`}
          aria-pressed={date === selected}
          aria-label={`${DAYS[i]} ${dateOf(date).getDate()}`}
          onClick={() => onSelect(date)}
        >
          <span className="ws-name">{LETTERS[i]}</span>
          <span className="ws-num">{dateOf(date).getDate()}</span>
          <span className="ws-col">
            {occurrencesOn(entries, date)
              .filter((o) => !o.skipped)
              .map((o) => (
                <span
                  key={o.entry.id}
                  className={`ws-seg ${o.entry.kind}`}
                  data-tip={`${spanOf(o)} ${o.entry.title}`}
                  style={{
                    top: `${(((o.start - START) / (END - START)) * 100).toFixed(1)}%`,
                    height: `${Math.max(6, ((o.end - o.start) / (END - START)) * 100).toFixed(1)}%`,
                  }}
                />
              ))}
          </span>
        </button>
      ))}
    </div>
  )
}

// The day as ground: every window of it named on demand, the disc where now stands,
// and the task blocks the plan laid over its free time.
function DayBands({
  date,
  today,
  now,
  entries,
  blocks,
  quietUntil,
  onBlock,
}: {
  date: string
  today: string
  now: number
  entries: CalendarEntry[]
  blocks: PlanEvent[]
  quietUntil: string | null
  onBlock: (event: PlanEvent) => void
}) {
  const isToday = date === today
  return (
    <div className="dayline" role="img" aria-label="The day, drawn as a line">
      <span className="dl-line" />
      {isToday && <span className="dl-gone" style={{ width: pct(now) }} />}
      <span className="dl-ticks" />
      {occurrencesOn(entries, date).map((o) =>
        o.entry.kind === 'note' ? (
          <span
            key={o.entry.id}
            className="dl-note cal-band"
            data-tip={`${spanOf(o)} ${o.entry.title}`}
            data-tip-skipped={o.skipped ? '' : undefined}
            style={{ left: pct(o.start) }}
          />
        ) : (
          <span
            key={o.entry.id}
            className={bandClass(o, 'dl-band cal-band')}
            data-tip={`${spanOf(o)} ${o.entry.title}`}
            data-tip-skipped={o.skipped ? '' : undefined}
            style={{
              left: pct(o.start),
              width: `${Math.max(0.6, ((o.end - o.start) / (END - START)) * 100)}%`,
            }}
          />
        ),
      )}
      {blocks.map((ev) => {
        const start = minutesOf(ev.wall_time)
        const end = minutesOf(ev.end_wall_time ?? ev.wall_time)
        return (
          <button
            key={ev.id}
            className={`dl-span task cal-band${ev.status === 'done' || ev.status === 'dropped' ? ' settled' : ''}`}
            data-tip={`${ev.wall_time} – ${ev.end_wall_time ?? ev.wall_time} ${ev.task?.title ?? ev.kind}${ev.origin === 'auto' ? ' · auto' : ''}`}
            aria-label={`${ev.task?.title ?? ev.kind} ${ev.wall_time}`}
            style={{ left: pct(start), width: `${Math.max(0.6, ((end - start) / (END - START)) * 100)}%` }}
            onClick={() => onBlock(ev)}
          />
        )
      })}
      {HOURS.map((h, i) => (
        <span
          key={h}
          className={`dl-hour${i === 0 ? ' edge-start' : i === HOURS.length - 1 ? ' edge-end' : ''}`}
          style={{ left: pct(h * 60) }}
        >
          {pad(h)}
        </span>
      ))}
      {isToday && (
        <span className="dl-now" style={{ left: pct(now) }}>
          {quietUntil && (
            <span className="dl-label quiet cal-quiet">
              <BellOff />
              <span>until {quietUntil}</span>
            </span>
          )}
        </span>
      )}
    </div>
  )
}

function DayList({
  date,
  entries,
  onPick,
}: {
  date: string
  entries: CalendarEntry[]
  onPick: (entry: CalendarEntry) => void
}) {
  const occ = occurrencesOn(entries, date)
  if (!occ.length) return <p className="cal-empty">Nothing on this day.</p>
  return (
    <ul className="cal-list">
      {occ.map((o) => (
        <li key={o.entry.id} className={o.skipped ? 'skipped' : undefined}>
          <span className="home-when">{spanOf(o)}</span>
          <button onClick={() => onPick(o.entry)}>{o.entry.title}</button>
          {o.entry.quiet && !o.skipped ? <BellOff /> : <span />}
        </li>
      ))}
    </ul>
  )
}

function WeekGrid({
  week,
  today,
  now,
  entries,
  blocks,
  quietUntil,
  onPick,
  onBlock,
}: {
  week: string[]
  today: string
  now: number
  entries: CalendarEntry[]
  blocks: Record<string, PlanEvent[]>
  quietUntil: string | null
  onPick: (entry: CalendarEntry, date: string) => void
  onBlock: (event: PlanEvent) => void
}) {
  const hours = Array.from({ length: 19 }, (_, i) => i + 6)
  const thisWeek = week.includes(today)
  return (
    <div className="week">
      <div className="week-head">
        <span />
        {week.map((date, i) => (
          <span
            key={date}
            className={`ws-day${date === today ? ' is-today' : date < today ? ' past' : ''}`}
          >
            <span className="ws-name">{LETTERS[i]}</span>
            <span className="ws-num">{dateOf(date).getDate()}</span>
          </span>
        ))}
      </div>
      <div className="week-body">
        {hours.map((h) => (
          <span
            key={h}
            className={`week-rule${h % 3 === 0 ? ' major' : ''}`}
            style={{ top: `calc(var(--hour) * ${h - 6})` }}
          />
        ))}
        <div className="week-gutter">
          {HOURS.map((h) => (
            <span key={h} className="week-hour">
              <span style={{ top: `calc(var(--hour) * ${h - 6})` }}>{pad(h)}</span>
            </span>
          ))}
        </div>
        {week.map((date) => (
          <div key={date} className={`week-col${date === today ? ' is-today' : ''}`}>
            {occurrencesOn(entries, date).map((o) => {
              const height = Math.max(0.5, (o.end - o.start) / 60)
              const tall = o.entry.kind !== 'note' && height >= 3
              return (
                <div
                  key={o.entry.id}
                  className={`${bandClass(o, 'band')}${tall ? ' tall' : ''}`}
                  role="button"
                  tabIndex={0}
                  data-entry={o.entry.id}
                  data-tip={spanOf(o)}
                  data-tip-skipped={o.skipped ? '' : undefined}
                  aria-label={`${o.entry.title} ${spanOf(o)}`}
                  style={{
                    top: `calc(var(--hour) * ${(o.start - START) / 60})`,
                    height: `calc(var(--hour) * ${height})`,
                  }}
                  onClick={() => onPick(o.entry, date)}
                  onKeyDown={(e) => {
                    if (e.key !== 'Enter' && e.key !== ' ') return
                    e.preventDefault()
                    onPick(o.entry, date)
                  }}
                >
                  <span className="t">{o.entry.title}</span>
                </div>
              )
            })}
            {(blocks[date] ?? []).map((ev) => {
              const start = minutesOf(ev.wall_time)
              const height = Math.max(0.5, (minutesOf(ev.end_wall_time ?? ev.wall_time) - start) / 60)
              return (
                <div
                  key={`b${ev.id}`}
                  className={`band task${ev.status === 'done' || ev.status === 'dropped' ? ' settled' : ''}`}
                  role="button"
                  tabIndex={0}
                  data-tip={`${ev.wall_time} – ${ev.end_wall_time ?? ev.wall_time}${ev.origin === 'auto' ? ' · auto' : ''}`}
                  aria-label={`${ev.task?.title ?? ev.kind} ${ev.wall_time}`}
                  style={{
                    top: `calc(var(--hour) * ${(start - START) / 60})`,
                    height: `calc(var(--hour) * ${height})`,
                  }}
                  onClick={() => onBlock(ev)}
                  onKeyDown={(e) => {
                    if (e.key !== 'Enter' && e.key !== ' ') return
                    e.preventDefault()
                    onBlock(ev)
                  }}
                >
                  <span className="t">{ev.task?.title ?? ev.kind}</span>
                </div>
              )
            })}
          </div>
        ))}
        {thisWeek && (
          <span className="week-now" style={{ top: `calc(var(--hour) * ${(now - START) / 60})` }}>
            {quietUntil && (
              <span className="cal-quiet">
                <BellOff />
                <span>until {quietUntil}</span>
              </span>
            )}
          </span>
        )}
      </div>
    </div>
  )
}

type Draft = {
  title: string
  kind: CalendarKind
  quiet: boolean
  start: string
  end: string
  days: number
  date: string
}

function EntrySheet({
  entry,
  date,
  onClose,
  onSave,
  onDelete,
  onSkip,
}: {
  entry: CalendarEntry | null
  date: string
  onClose: () => void
  onSave: (draft: Draft) => void
  onDelete: () => void
  onSkip: () => void
}) {
  const [draft, setDraft] = useState<Draft>(() => ({
    title: entry?.title ?? '',
    kind: entry?.kind ?? 'fixed',
    quiet: entry?.quiet ?? true,
    start: entry?.start_time ?? '16:00',
    end: entry?.end_time ?? '17:00',
    days: entry?.days ?? 0,
    date: entry?.on_date ?? date,
  }))
  const [menu, setMenu] = useState(false)
  const scrim = useRef<HTMLDivElement>(null)
  const panel = useRef<HTMLDivElement>(null)
  const title = useRef<HTMLInputElement>(null)
  const closing = useRef(false)

  useEffect(() => {
    if (!entry) title.current?.focus({ preventScroll: true })
    if (reducedMotion()) return
    gsap.from(scrim.current, { autoAlpha: 0, duration: 0.3, ease: 'power2.out' })
    gsap.from(panel.current, {
      y: 48,
      autoAlpha: 0,
      duration: 0.4,
      ease: 'expo.out',
      clearProps: 'transform,opacity,visibility',
    })
  }, [entry])

  const close = useCallback(() => {
    if (closing.current) return
    closing.current = true
    if (reducedMotion()) return onClose()
    gsap.to(scrim.current, { autoAlpha: 0, duration: 0.25, ease: 'power2.in' })
    gsap.to(panel.current, {
      y: 48,
      autoAlpha: 0,
      duration: 0.28,
      ease: 'power2.in',
      onComplete: onClose,
    })
  }, [onClose])
  useEscape(true, close)

  const set = (patch: Partial<Draft>) => setDraft((d) => ({ ...d, ...patch }))
  const toggleDay = (i: number) => set({ days: draft.days ^ (1 << i) })
  const ready =
    draft.title.trim().length > 0 &&
    TIME.test(draft.start) &&
    TIME.test(draft.end) &&
    draft.end > draft.start

  return createPortal(
    <>
      <div className="scrim" ref={scrim} onClick={() => close()} />
      <div
        className="sheet"
        role="dialog"
        aria-modal="true"
        aria-label={entry ? entry.title : 'New entry'}
        ref={panel}
      >
        <div className="sheet-handle" />
        <div className="sheet-top">
          <input
            className="sheet-title"
            ref={title}
            value={draft.title}
            placeholder="Title"
            aria-label="Title"
            onChange={(e) => set({ title: e.target.value })}
          />
          {entry && (
            <div className="ev-more-wrap">
              <button
                className="ev-more"
                aria-label="More"
                aria-haspopup="menu"
                aria-expanded={menu}
                onClick={() => setMenu((v) => !v)}
              >
                ⋯
              </button>
              {menu && (
                <SheetMenu
                  kind={draft.kind}
                  onKind={(kind) => {
                    setMenu(false)
                    set({ kind, quiet: kind === 'note' || kind === 'free' ? false : draft.quiet })
                  }}
                  onSkip={() => {
                    onSkip()
                    close()
                  }}
                  onDelete={() => {
                    onDelete()
                    close()
                  }}
                  onClose={() => setMenu(false)}
                />
              )}
            </div>
          )}
        </div>
        <div className="sheet-time">
          <input
            className="pill-in tnum"
            inputMode="numeric"
            maxLength={5}
            aria-label="Start"
            value={draft.start}
            onChange={(e) => set({ start: e.target.value })}
            onBlur={(e) => set({ start: asTime(e.target.value) })}
          />
          <span>–</span>
          <input
            className="pill-in tnum"
            inputMode="numeric"
            maxLength={5}
            aria-label="End"
            value={draft.end}
            onChange={(e) => set({ end: e.target.value })}
            onBlur={(e) => set({ end: asTime(e.target.value) })}
          />
        </div>
        <div className="days" role="group" aria-label="Days">
          {LETTERS.map((letter, i) => (
            <button
              key={DAYS[i]}
              className="day-dot"
              aria-pressed={(draft.days & (1 << i)) !== 0}
              aria-label={DAYS[i]}
              onClick={() => toggleDay(i)}
            >
              {letter}
            </button>
          ))}
        </div>
        <div className="sheet-row">
          {draft.kind === 'note' ? (
            <span className="label">A mark on the day, not a window</span>
          ) : draft.kind === 'free' ? (
            <span className="label">Time set aside for tasks</span>
          ) : (
            <span className="label">
              <BellOff />
              Quiet
            </span>
          )}
          {draft.days === 0 && (
            <span className="sheet-date">{`${DAYS[weekdayOf(draft.date)]} ${dateOf(draft.date).getDate()} ${MONTHS[dateOf(draft.date).getMonth()].slice(0, 3)}`}</span>
          )}
          {draft.kind !== 'note' && draft.kind !== 'free' && (
            <button
              className="sw"
              role="switch"
              aria-checked={draft.quiet}
              aria-label="Quiet"
              onClick={() => set({ quiet: !draft.quiet })}
            />
          )}
        </div>
        <button
          className="btn-fill wide sheet-save"
          disabled={!ready}
          onClick={() => {
            onSave(draft)
            close()
          }}
        >
          Save
        </button>
      </div>
    </>,
    document.body,
  )
}

// A task block is the plan's, not the calendar's: clicking one offers the row's
// own actions rather than the entry sheet.
function BlockMenu({
  event,
  onStart,
  onDone,
  onDrop,
  onMove,
  onClose,
}: {
  event: PlanEvent
  onStart: () => void
  onDone: () => void
  onDrop: () => void
  onMove: () => void
  onClose: () => void
}) {
  useEscape(true, onClose)
  const settled = event.status === 'done' || event.status === 'dropped'
  return createPortal(
    <>
      <div className="scrim" onClick={onClose} />
      <div className="block-menu" role="dialog" aria-modal="true" aria-label={event.task?.title ?? event.kind}>
        <span className="block-menu-head">
          <span className="block-menu-title">{event.task?.title ?? event.kind}</span>
          <span className="block-menu-when tnum">
            {event.wall_time} – {event.end_wall_time ?? event.wall_time}
            {event.origin === 'auto' && <span className="block-menu-auto">auto</span>}
          </span>
        </span>
        {!settled && (
          <>
            <button className="ev-menu-item" onClick={onStart}>Start</button>
            <button className="ev-menu-item" onClick={onDone}>Done</button>
            <div className="ev-menu-sep" />
            <button className="ev-menu-item" onClick={onDrop}>Drop today</button>
            <button className="ev-menu-item" onClick={onMove}>Move to tomorrow</button>
          </>
        )}
        {settled && <p className="block-menu-settled">Already {event.status}.</p>}
      </div>
    </>,
    document.body,
  )
}

function SheetMenu({
  kind,
  onKind,
  onSkip,
  onDelete,
  onClose,
}: {
  kind: CalendarKind
  onKind: (kind: CalendarKind) => void
  onSkip: () => void
  onDelete: () => void
  onClose: () => void
}) {
  const wrap = useRef<HTMLDivElement>(null)
  const first = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    first.current?.focus()
    if (!reducedMotion()) {
      gsap.from(wrap.current, {
        autoAlpha: 0,
        y: 6,
        scale: 0.97,
        transformOrigin: 'bottom right',
        duration: 0.24,
        ease: 'power2.out',
        clearProps: 'transform,opacity,visibility',
      })
    }
    const onDown = (e: globalThis.MouseEvent) => {
      if (!wrap.current?.parentElement?.contains(e.target as Node)) onClose()
    }
    document.addEventListener('mousedown', onDown)
    return () => document.removeEventListener('mousedown', onDown)
  }, [onClose])

  return (
    <div className="ev-menu up" role="menu" ref={wrap}>
      {KINDS.map((k, i) => (
        <button
          key={k}
          className="ev-menu-item"
          role="menuitemradio"
          aria-checked={k === kind}
          ref={i === 0 ? first : undefined}
          onClick={() => onKind(k)}
        >
          {KIND_LABEL[k]}
          {k === kind && <Check />}
        </button>
      ))}
      <div className="ev-menu-sep" />
      <button className="ev-menu-item" role="menuitem" onClick={onSkip}>
        Skip this day
      </button>
      <button className="ev-menu-item" role="menuitem" onClick={onDelete}>
        Delete
      </button>
    </div>
  )
}

// One label at a time: anything with data-tip says its words above itself on hover,
// and a tap pins them until the next tap elsewhere.
function useTips() {
  const [anchor, setAnchor] = useState<HTMLElement | null>(null)
  const [pinned, setPinned] = useState<HTMLElement | null>(null)
  const [, nudge] = useState(0)
  const tip = useRef<HTMLDivElement>(null)

  useLayoutEffect(() => {
    const el = tip.current
    if (!el || !anchor) return
    const r = anchor.getBoundingClientRect()
    const x = Math.min(
      Math.max(8, r.left + r.width / 2 - el.offsetWidth / 2),
      window.innerWidth - el.offsetWidth - 8,
    )
    el.style.left = `${x + window.scrollX}px`
    el.style.top = `${r.top + window.scrollY - el.offsetHeight - 6}px`
  })

  useEffect(() => {
    if (!anchor) return
    const on = () => nudge((n) => n + 1)
    window.addEventListener('scroll', on, { passive: true })
    window.addEventListener('resize', on)
    return () => {
      window.removeEventListener('scroll', on)
      window.removeEventListener('resize', on)
    }
  }, [anchor])

  const marked = (e: ReactMouseEvent) => (e.target as HTMLElement).closest<HTMLElement>('[data-tip]')

  return {
    over: (e: ReactMouseEvent) => {
      const el = marked(e)
      if (el && !pinned) setAnchor(el)
    },
    out: () => {
      if (!pinned) setAnchor(null)
    },
    click: (e: ReactMouseEvent) => {
      const el = marked(e)
      // A band that opens the sheet keeps its tap for the sheet.
      if (!el || el.dataset.entry !== undefined || el === pinned) {
        setPinned(null)
        setAnchor(null)
        return
      }
      setPinned(el)
      setAnchor(el)
    },
    node: anchor
      ? createPortal(
          <div className="tip on" ref={tip} aria-hidden="true">
            {anchor.dataset.tipSkipped !== undefined ? (
              <s>{anchor.dataset.tip}</s>
            ) : (
              anchor.dataset.tip
            )}
          </div>,
          document.body,
        )
      : null,
  }
}

function Plus() {
  return (
    <svg className="glyph" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 5v14M5 12h14" />
    </svg>
  )
}

function ChevronRight() {
  return (
    <svg className="glyph cal-chev" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M9 6l6 6-6 6" />
    </svg>
  )
}

function Check() {
  return (
    <svg className="glyph check" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M5 12.5l4.5 4.5L19 7" />
    </svg>
  )
}

function BellOff() {
  return (
    <svg className="glyph" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6.5 16.5V11a5.5 5.5 0 0 1 8.2-4.8" />
      <path d="M17.5 11v5.5l1.5 2h-14l1.5-2" />
      <path d="M10 21h4" />
      <path d="M4 4l16 16" />
    </svg>
  )
}
