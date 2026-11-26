import { useContext, useEffect, useState, type ReactNode } from 'react'
import { t, type Key } from '../../i18n'
import * as format from '../../i18n/format'
import type { Delivery, Patience, ReplyLength, ReplyTone, ReviewDay, Tuning } from '../../types'
import { FoldRow, Status, ToggleRow } from '../../ui/rows'
import { SettingsContext } from './ToggleSetting'

type NumberKey = { [K in keyof Tuning]: Tuning[K] extends number ? K : never }[keyof Tuning]
type FlagKey = { [K in keyof Tuning]: Tuning[K] extends boolean ? K : never }[keyof Tuning]
type Fold = { open: boolean; onToggle: () => void }
type Choice<T extends string> = { id: T; key: Key }

// The ranges the server holds each value to.
const RANGE: Record<NumberKey, [number, number]> = {
  overrun_ask_pct: [110, 500],
  overrun_end_pct: [120, 1000],
  gone_grace_min: [1, 60],
  max_pause_min: [10, 480],
  first_check_min: [5, 120],
  block_min: [5, 240],
  spoke_hold_min: [10, 480],
  idle_backoff_steps: [0, 6],
  no_show_grace_min: [0, 240],
  close_day_calls_min: [0, 180],
  ring_secs: [10, 120],
  due_soon_days: [1, 14],
  soon_days: [1, 30],
  lead_min: [0, 60],
  near_min: [5, 120],
  tidy_stale_days: [3, 90],
  tidy_overdue_days: [1, 30],
  tidy_big_min: [30, 480],
  tidy_small_min: [5, 120],
  note_stale_days: [1, 30],
  learn_window_days: [3, 60],
  history_limit: [8, 128],
  lookup_rounds: [1, 10],
  completion_cap: [20_000, 400_000],
  working_memory_cap: [10, 200],
}

const REVIEW_DAYS: ReviewDay[] = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun']
// 2026-11-23 is a Monday.
const weekdayName = (i: number) => format.weekday(new Date(2026, 10, 23 + i))

const PATIENCE: Choice<Patience>[] = [
  { id: 'quick', key: 'tuning.patience.quick' },
  { id: 'normal', key: 'tuning.normal' },
  { id: 'patient', key: 'tuning.patience.patient' },
]
const LENGTHS: Choice<ReplyLength>[] = [
  { id: 'short', key: 'tuning.length.short' },
  { id: 'normal', key: 'tuning.normal' },
  { id: 'detailed', key: 'tuning.length.detailed' },
]
const TONES: Choice<ReplyTone>[] = [
  { id: 'default', key: 'tuning.normal' },
  { id: 'warm', key: 'tuning.tone.warm' },
  { id: 'direct', key: 'tuning.tone.direct' },
  { id: 'playful', key: 'tuning.tone.playful' },
]
const DELIVERY: Choice<Delivery>[] = [
  { id: 'push', key: 'announce.notify' },
  { id: 'chat', key: 'announce.chat' },
]
const CHANNELS: { field: 'nudge_channel' | 'checkin_channel' | 'review_channel'; key: Key }[] = [
  { field: 'checkin_channel', key: 'tuning.channel.checkin' },
  { field: 'nudge_channel', key: 'tuning.channel.nudge' },
  { field: 'review_channel', key: 'tuning.channel.review' },
]

const labelOf = <T extends string>(list: Choice<T>[], id: T) => t(list.find((c) => c.id === id)?.key ?? list[0].key)

function useTuning(row: string) {
  const ctx = useContext(SettingsContext)
  if (!ctx) return null
  return {
    settings: ctx.settings,
    tuning: ctx.settings.tuning,
    busy: ctx.busy,
    save: ctx.save,
    put: (patch: Partial<Tuning>) => ctx.saveSettings(row, { tuning: patch }),
    putDayStart: (day_start: string) => ctx.saveSettings(row, { day_start }),
  }
}

// Holds what is typed until it leaves the field; a value the server refuses springs back.
function Field({
  label,
  value,
  type,
  min,
  max,
  disabled,
  onCommit,
}: {
  label: string
  value: string
  type: 'number' | 'time'
  min?: number
  max?: number
  disabled: boolean
  onCommit: (raw: string) => Promise<boolean>
}) {
  const [draft, setDraft] = useState(value)
  useEffect(() => setDraft(value), [value])
  const commit = async () => {
    if (draft === value) return
    if (!(await onCommit(draft))) setDraft(value)
  }
  return (
    <label className="set-tune">
      <span className="set-sub">{label}</span>
      <input
        type={type}
        min={min}
        max={max}
        value={draft}
        disabled={disabled}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={() => void commit()}
        onKeyDown={(e) => {
          if (e.key === 'Enter') e.currentTarget.blur()
        }}
      />
    </label>
  )
}

function Seg<T extends string>({
  label,
  value,
  options,
  disabled,
  onPick,
}: {
  label: string
  value: T
  options: { id: T; label: string }[]
  disabled: boolean
  onPick: (id: T) => void
}) {
  return (
    <div className="seg" role="group" aria-label={label}>
      {options.map((o) => (
        <button key={o.id} type="button" aria-pressed={value === o.id} disabled={disabled} onClick={() => onPick(o.id)}>
          {o.label}
        </button>
      ))}
    </div>
  )
}

function Head({ children }: { children: ReactNode }) {
  return <span className="set-tune-head">{children}</span>
}

type Tuner = NonNullable<ReturnType<typeof useTuning>>

function NumberTune({ tuner, field, label }: { tuner: Tuner; field: NumberKey; label: Key }) {
  const [min, max] = RANGE[field]
  return (
    <Field
      label={t(label)}
      type="number"
      min={min}
      max={max}
      value={String(tuner.tuning[field])}
      disabled={tuner.busy}
      onCommit={(raw) => {
        const n = Math.round(Number(raw))
        if (!Number.isFinite(n) || raw.trim() === '') return Promise.resolve(false)
        return tuner.put({ [field]: n })
      }}
    />
  )
}

function FlagTune({ tuner, field, label }: { tuner: Tuner; field: FlagKey; label: Key }) {
  const on = tuner.tuning[field]
  return <ToggleRow label={t(label)} on={on} disabled={tuner.busy} onToggle={() => void tuner.put({ [field]: !on })} />
}

const timeOrMidnight = (wall: string) => (wall === '00:00' ? t('settings.callsUntil.midnight') : wall)

export function DayBoundsRow({ open, onToggle }: Fold) {
  const tuner = useTuning('day_bounds')
  if (!tuner) return null
  const { settings, tuning } = tuner
  return (
    <FoldRow
      label={t('tuning.dayBounds')}
      value={`${settings.day_start || t('settings.off')} – ${timeOrMidnight(tuning.day_end)}`}
      open={open}
      onToggle={onToggle}
    >
      {open && (
        <div className="set-fold-body">
          <div className="set-tunes">
            <Field
              label={t('tuning.dayStart')}
              type="time"
              value={settings.day_start}
              disabled={tuner.busy}
              onCommit={(raw) => tuner.putDayStart(raw)}
            />
            <Field
              label={t('tuning.dayEnd')}
              type="time"
              value={tuning.day_end}
              disabled={tuner.busy}
              onCommit={(raw) => tuner.put({ day_end: raw || '00:00' })}
            />
          </div>
          <span className="set-sub">{t('tuning.dayEnd.sub')}</span>
          <Status save={tuner.save} row="day_bounds" />
        </div>
      )}
    </FoldRow>
  )
}

export function ReviewRow({ open, onToggle }: Fold) {
  const tuner = useTuning('review')
  if (!tuner) return null
  const day = tuner.tuning.review_day
  const options = [
    ...REVIEW_DAYS.map((id, i) => ({ id, label: weekdayName(i) })),
    { id: 'off' as const, label: t('settings.off') },
  ]
  return (
    <FoldRow
      label={t('tuning.review')}
      value={options.find((o) => o.id === day)?.label}
      open={open}
      onToggle={onToggle}
    >
      {open && (
        <div className="set-fold-body">
          <Seg label={t('tuning.review')} value={day} options={options} disabled={tuner.busy} onPick={(review_day) => void tuner.put({ review_day })} />
          <Status save={tuner.save} row="review" />
        </div>
      )}
    </FoldRow>
  )
}

export function SessionLimitsRow({ open, onToggle }: Fold) {
  const tuner = useTuning('session_limits')
  if (!tuner) return null
  const { tuning } = tuner
  return (
    <FoldRow
      label={t('tuning.limits')}
      value={tuning.overrun_auto_end ? `${tuning.overrun_ask_pct}% · ${tuning.overrun_end_pct}%` : `${tuning.overrun_ask_pct}%`}
      open={open}
      onToggle={onToggle}
    >
      {open && (
        <div className="set-fold-body">
          <Head>{t('tuning.head.overrun')}</Head>
          <div className="set-tunes">
            <NumberTune tuner={tuner} field="overrun_ask_pct" label="tuning.overrunAsk" />
            {tuning.overrun_auto_end && <NumberTune tuner={tuner} field="overrun_end_pct" label="tuning.overrunEnd" />}
          </div>
          <FlagTune tuner={tuner} field="overrun_auto_end" label="tuning.overrunAutoEnd" />
          <Head>{t('tuning.head.away')}</Head>
          <div className="set-tunes">
            <NumberTune tuner={tuner} field="gone_grace_min" label="tuning.goneGrace" />
            <NumberTune tuner={tuner} field="max_pause_min" label="tuning.maxPause" />
          </div>
          <Head>{t('tuning.head.lengths')}</Head>
          <div className="set-tunes">
            <NumberTune tuner={tuner} field="first_check_min" label="tuning.firstCheck" />
            {!tuning.block_follows_pomodoro && <NumberTune tuner={tuner} field="block_min" label="tuning.blockMin" />}
          </div>
          <FlagTune tuner={tuner} field="block_follows_pomodoro" label="tuning.followPomodoro" />
          <Status save={tuner.save} row="session_limits" />
        </div>
      )}
    </FoldRow>
  )
}

export function ReplyRow({ open, onToggle }: Fold) {
  const tuner = useTuning('reply')
  if (!tuner) return null
  const { tuning } = tuner
  const choices = <T extends string>(list: Choice<T>[]) => list.map((c) => ({ id: c.id, label: t(c.key) }))
  return (
    <FoldRow
      label={t('tuning.reply')}
      value={`${labelOf(LENGTHS, tuning.reply_length)} · ${labelOf(TONES, tuning.reply_tone)}`}
      open={open}
      onToggle={onToggle}
    >
      {open && (
        <div className="set-fold-body">
          <Seg label={t('tuning.reply.length')} value={tuning.reply_length} options={choices(LENGTHS)} disabled={tuner.busy} onPick={(reply_length) => void tuner.put({ reply_length })} />
          <Seg label={t('tuning.reply.tone')} value={tuning.reply_tone} options={choices(TONES)} disabled={tuner.busy} onPick={(reply_tone) => void tuner.put({ reply_tone })} />
          <Status save={tuner.save} row="reply" />
        </div>
      )}
    </FoldRow>
  )
}

export function PatienceRow({ open, onToggle }: Fold) {
  const tuner = useTuning('patience')
  if (!tuner) return null
  const { tuning } = tuner
  return (
    <FoldRow label={t('tuning.patience')} value={labelOf(PATIENCE, tuning.patience)} open={open} onToggle={onToggle}>
      {open && (
        <div className="set-fold-body">
          <Seg
            label={t('tuning.patience')}
            value={tuning.patience}
            options={PATIENCE.map((c) => ({ id: c.id, label: t(c.key) }))}
            disabled={tuner.busy}
            onPick={(patience) => void tuner.put({ patience })}
          />
          <Status save={tuner.save} row="patience" />
        </div>
      )}
    </FoldRow>
  )
}

export function DeliveryRow({ open, onToggle }: Fold) {
  const tuner = useTuning('delivery')
  if (!tuner) return null
  const { tuning } = tuner
  const options = DELIVERY.map((c) => ({ id: c.id, label: t(c.key) }))
  const quiet = CHANNELS.filter((c) => tuning[c.field] === 'chat').length
  return (
    <FoldRow
      label={t('tuning.delivery')}
      value={quiet ? t('tuning.delivery.chatOnly', { n: quiet }) : t('announce.notify')}
      open={open}
      onToggle={onToggle}
    >
      {open && (
        <div className="set-fold-body">
          {CHANNELS.map((c) => (
            <div className="set-row" key={c.field}>
              <span className="set-row-body">
                <span className="set-label">{t(c.key)}</span>
              </span>
              <Seg label={t(c.key)} value={tuning[c.field]} options={options} disabled={tuner.busy} onPick={(v) => void tuner.put({ [c.field]: v })} />
            </div>
          ))}
          <Status save={tuner.save} row="delivery" />
        </div>
      )}
    </FoldRow>
  )
}

export function FineTuningRow({ open, onToggle }: Fold) {
  const tuner = useTuning('fine')
  if (!tuner) return null
  const { tuning } = tuner
  return (
    <FoldRow label={t('tuning.fine')} open={open} onToggle={onToggle}>
      {open && (
        <div className="set-fold-body">
          <Head>{t('tuning.head.wake')}</Head>
          <div className="set-tunes">
            <Field
              label={t('tuning.wakeEarliest')}
              type="time"
              value={tuning.wake_earliest}
              disabled={tuner.busy}
              onCommit={(raw) => tuner.put({ wake_earliest: raw })}
            />
            <NumberTune tuner={tuner} field="close_day_calls_min" label="tuning.closeDayCalls" />
            <NumberTune tuner={tuner} field="ring_secs" label="tuning.ringSecs" />
          </div>
          <Head>{t('tuning.head.nudges')}</Head>
          <div className="set-tunes">
            <NumberTune tuner={tuner} field="no_show_grace_min" label="tuning.noShowGrace" />
            <NumberTune tuner={tuner} field="spoke_hold_min" label="tuning.spokeHold" />
            <NumberTune tuner={tuner} field="idle_backoff_steps" label="tuning.backoff" />
          </div>
          <Head>{t('tuning.head.tasks')}</Head>
          <div className="set-tunes">
            <NumberTune tuner={tuner} field="soon_days" label="tuning.soonDays" />
            <NumberTune tuner={tuner} field="due_soon_days" label="tuning.dueSoon" />
            <NumberTune tuner={tuner} field="lead_min" label="tuning.lead" />
            <NumberTune tuner={tuner} field="near_min" label="tuning.near" />
          </div>
          <FlagTune tuner={tuner} field="calendar_quiet" label="tuning.calendarQuiet" />
          <Head>{t('tuning.head.tidy')}</Head>
          <div className="set-tunes">
            <NumberTune tuner={tuner} field="tidy_stale_days" label="tuning.tidyStale" />
            <NumberTune tuner={tuner} field="tidy_overdue_days" label="tuning.tidyOverdue" />
            <NumberTune tuner={tuner} field="tidy_small_min" label="tuning.tidySmall" />
            {tuning.auto_breakdown && <NumberTune tuner={tuner} field="tidy_big_min" label="tuning.tidyBig" />}
            <NumberTune tuner={tuner} field="note_stale_days" label="tuning.noteStale" />
          </div>
          <FlagTune tuner={tuner} field="auto_breakdown" label="tuning.breakdown" />
          <Head>{t('tuning.head.ai')}</Head>
          <FlagTune tuner={tuner} field="learning" label="tuning.learning" />
          <div className="set-tunes">
            {tuning.learning && <NumberTune tuner={tuner} field="learn_window_days" label="tuning.learnWindow" />}
            <NumberTune tuner={tuner} field="history_limit" label="tuning.history" />
            <NumberTune tuner={tuner} field="lookup_rounds" label="tuning.lookups" />
            <NumberTune tuner={tuner} field="working_memory_cap" label="tuning.workingMemory" />
            <NumberTune tuner={tuner} field="completion_cap" label="tuning.completionCap" />
          </div>
          <Status save={tuner.save} row="fine" />
        </div>
      )}
    </FoldRow>
  )
}
