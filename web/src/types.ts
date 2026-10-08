// `onboarding` holds from joining by invite until the first-run flow is finished.
export type Me = { username: string; admin: boolean; onboarding?: boolean; voice?: boolean }

export type JoinInfo = { username: string | null; expires_at: string }

export type Invite = {
  id: number
  admin: boolean
  username: string | null
  created_by: string | null
  created_at: string
  expires_at: string
}

// The only response that carries the link.
export type InviteCreated = Invite & { token: string; url: string }

export type MovedTo = { event_id: number; date: string; wall_time: string; kind: string }

// Where the event came from: the template, the agent, the allocator, or a user edit.
export type EventOrigin = 'template' | 'agent' | 'auto' | 'user' | 'idle' | 'lay_day'

export type TaskUrgency = 'low' | 'normal' | 'high'

// The task a block holds, carried so the block can offer the task's own actions.
// `title` is always the top-level task; `step` names the one this block holds.
export type TaskRef = {
  id: number
  title: string
  state: TaskState
  step: string | null
  category: string
  urgency: TaskUrgency
  pressing: boolean
  notify?: TaskNotify
}

export type PlanEvent = {
  id: number
  kind: string
  wall_time: string
  end_wall_time: string | null
  entry: 'routine' | 'block'
  status: 'pending' | 'fired' | 'snoozed' | 'done' | 'dropped'
  flexibility: 'fixed' | 'slide' | 'drop'
  slide_window_min: number
  channel: string
  alert: boolean
  origin: EventOrigin
  // What a trigger is meant to follow up on; absent on every other event.
  prompt?: string
  // RFC 3339 UTC; set once the event was finished, dropped, moved or snoozed
  decided_at?: string
  // present only when the agent named the event this one moved to
  moved_to?: MovedTo
  // present only on a block laid for a task
  task?: TaskRef
}

export type TaskState = 'open' | 'in_progress' | 'done' | 'dropped'

// How the block laid for a task announces itself when it starts.
export type TaskNotify = 'none' | 'chat' | 'notify'

export type Task = {
  id: number
  title: string
  description: string
  state: TaskState
  source: string
  notes: string
  duration_min: number | null
  duration_source: 'user' | 'agent' | 'none'
  parent_id: number | null
  is_now: boolean
  updated_at: string
  // RFC 3339 UTC, like updated_at; steps never carry one.
  due_at: string | null
  external_id: string | null
  url: string
  notify: TaskNotify
  // Free text, one per task; a step reports the one its parent carries.
  category: string
  // low, normal or high; a step reports its parent's.
  urgency: TaskUrgency
  // Due inside the next two days or already past due. Derived by the server.
  pressing: boolean
  // The goal this task belongs to; steps never carry one.
  goal_id: number | null
  goal_title: string | null
  // When the task's next block still to come starts, as a date-time in the
  // user's own zone; null when nothing is laid for it.
  scheduled_at: string | null
  // How far along it is, 0 to 100.
  progress: number
  // Where the minutes already worked say the task lands, and what is left of
  // that; both absent until there is progress and time behind it.
  expected_min: number | null
  remaining_min: number | null
}

// A top-level task with its steps; the list never nests deeper than this.
export type TaskNode = Task & { children: Task[] }

export type GoalState = 'open' | 'done' | 'dropped'

// `tasks` counts everything hanging from the goal that was not dropped;
// `next_*` name the unfinished one whose deadline comes first.
export type Goal = {
  id: number
  title: string
  description: string
  due_at: string | null
  state: GoalState
  created_at: string
  updated_at: string
  tasks: number
  done_tasks: number
  next_task_id: number | null
  next_task_title: string | null
  next_due_at: string | null
}

export type Token = {
  id: number
  name: string
  created_at: string
  last_used_at: string | null
}

// The secret is present only in the create response.
export type TokenCreated = Token & { token: string }

// `parent` arrives when finishing or reopening cascaded to it; `demoted_from_now`
// when the write pushed other tasks out of Now, newest first.
export type TaskUpdate = Task & { parent?: Task; demoted_from_now?: number[] }

export type NewStep = { title: string; duration_min: number }

export type FlattenResult = { task: TaskNode; removed: Task[] }

// Where the user last spoke to the thread from; Note answers there.
export type ConversationVia = 'web' | 'matrix' | 'voice'

export type Conversation = {
  id: number
  title: string
  // 'draft' until the server has written a title of its own; 'user' once renamed by hand
  title_kind: 'draft' | 'generated' | 'user'
  updated_at: string
  summary: string | null
  via: ConversationVia
}

export type TalkStep = {
  name: string
  args: string
  result: string
  is_error: boolean
  // the reasoning of the round that made this call, on the round's first step only
  thinking: string | null
}

// `reasoning` holds the thinking of the round the row ended: on a tool row the
// round that made the call, on an assistant row the round that answered.
// `thought_ms` is set on the assistant row only. Both are null on rows written
// before the transcript kept them.
export type TalkMessage = {
  id: number
  role: 'user' | 'assistant' | 'tool'
  content: string
  tool_name: string | null
  tool_args: string | null
  is_error: boolean
  created_at: string
  reasoning: string | null
  thought_ms: number | null
}

export type TalkReply = {
  conversation_id: number
  reply: string
  steps: TalkStep[]
  reasoning: string
  thought_ms: number
}

// One entry of the day's template, in template order; `index` addresses it in a write.
export type ScheduleRow = {
  index: number
  kind: string
  entry: 'routine' | 'block'
  time: string
  end_time: string | null
  days: string[]
  flexibility: 'fixed' | 'slide' | 'drop'
  slide_window_min: number
  channel: string
  alert: boolean
}

export type Settings = {
  display_name: string
  timezone: string
  timezone_auto: boolean
  nightly_time: string
  // When the close-the-day card appears; '' where the user turned it off.
  close_day_time: string
  // Until when an open morning brings the letter to the face, HH:MM.
  morning_until: string
  template: string
  templates: string[]
  timezones: string[]
  schedule: ScheduleRow[]
  show_arc_between_sessions: boolean
  counter: 'remaining' | 'elapsed'
  category: 'member' | 'test'
  nightly_enabled: boolean
  checkins_enabled: boolean
  // How many check-ins Note may start on its own in a day.
  triggers_per_day: number
  pomodoro_enabled: boolean
  pomodoro_work_min: number
  pomodoro_break_min: number
  // Gates the pomodoro phase messages and the "Time's up." at a session's planned end.
  session_end_notify: boolean
  // Minutes quiet before Note may nudge about open notes; 0 is off.
  idle_nudge_min: number
  // Whether this server can ring a phone; the Calls row hides without it.
  voice_enabled: boolean
  voice_link: { mxid: string; state: 'invited' | 'linked' } | null
  ring_for: RingFor
  // The call voice's id; '' for the language's default.
  voice_voice: string
  // Whether a call plays its ready and heard sounds.
  voice_cue: boolean
  // Whether this server answers the Matrix DM; the message switches hide without it.
  matrix_enabled: boolean
  // Whether Note's own messages are also posted to the DM, and whether they notify there.
  matrix_send: boolean
  matrix_ping: boolean
  // '' follows the browser.
  language: Language
}

export type Language = '' | 'en' | 'ja'

export type RingFor = 'urgent' | 'checkins' | 'never'

export type VoiceChoice = { id: string; label: string; backend?: string; slow?: boolean; credit?: string }

export type AlertPatch = { index: number; alert: boolean }

export type SettingsSaved = Pick<
  Settings,
  | 'display_name'
  | 'timezone'
  | 'timezone_auto'
  | 'nightly_time'
  | 'close_day_time'
  | 'morning_until'
  | 'template'
  | 'show_arc_between_sessions'
  | 'counter'
  | 'nightly_enabled'
  | 'checkins_enabled'
  | 'triggers_per_day'
  | 'pomodoro_enabled'
  | 'pomodoro_work_min'
  | 'pomodoro_break_min'
  | 'session_end_notify'
  | 'idle_nudge_min'
  | 'ring_for'
  | 'voice_voice'
  | 'voice_cue'
  | 'matrix_send'
  | 'matrix_ping'
  | 'language'
> & { schedule: ScheduleRow[] }

export type AboutDoc = { content: string }

export type PromptKind = 'talk' | 'trigger' | 'nightly' | 'call'

export type BuiltPrompt = { prompt: string; tools: string[] }

export type Passkey = {
  id: number
  name: string
  created_at: string
  last_used_at: string | null
}

// `webauthn_available` is false wherever browsers would refuse WebAuthn.
export type SecurityState = {
  passkeys: Passkey[]
  totp: { enabled: boolean; pending: boolean }
  webauthn_available: boolean
}

// The only response that carries the new secret.
export type TotpEnrolment = {
  secret_base32: string
  otpauth_uri: string
  issuer: string
  account: string
}

export type MemoryHit = { id: string; category: string; summary: string }

export type MemoryFact = {
  id: string
  category: string
  summary: string
  body: string
  created: string
  archived: boolean
  supersedes: string | null
  source?: string | null
}

export type InboxKind = 'announcement' | 'material'
export type InboxOutcome = 'remembered' | 'nothing' | 'task'

export type InboxRow = {
  id: number
  source_id: string
  kind: InboxKind
  title: string
  received_at: string
  outcome: InboxOutcome | null
}

export type InboxPage = { items: InboxRow[]; latest: string | null; refresh: boolean }

export type InboxItem = InboxRow & {
  body: string
  reason: string | null
  decided_at: string | null
  memories: { id: string; summary: string; archived: boolean }[]
}

export type LogRow = {
  id: number
  ts: string
  user_id: number | null
  kind: string
  detail: string
}

// `totp` is the former shape of this report, kept for one release.
export type AdminGate = {
  elevated: boolean
  expires_at?: string
  second_factor: 'passkey' | 'totp' | 'none'
  methods: { passkey: boolean; totp: boolean }
  require_second_factor: boolean
  totp: 'required' | 'password_only' | 'missing'
  inspect: boolean
}

export type AdminStatus = {
  version: string
  build: 'release' | 'dev-inspect'
  started_at: string
  uptime_s: number
  db_bytes: number
  users: number
  sessions: number
  push_subscriptions: number
  providers: {
    llm: { kind: string; model: string } | null
    embeddings: { kind: string; model: string } | null
  }
  webpush: boolean
  secrets: { admin_totp: boolean }
}

export type AdminUser = {
  id: number
  username: string
  role: 'admin' | 'member'
  disabled: boolean
  sessions: number
}

export type AdminLog = { rows: LogRow[]; kinds: string[] }

export type TraceOutcome = 'ok' | 'max_turns' | 'error'

// Why the assistant could not finish a request; the server's `failure::Reason`.
export type FailureReason =
  | 'model_unavailable'
  | 'auth_invalid'
  | 'out_of_credits'
  | 'rate_limited'
  | 'provider_down'
  | 'context_too_long'
  | 'refused'
  | 'internal'

// `error` carries the failure only when `outcome` is 'error'.
export type TraceRow = {
  id: number
  ts: string
  user_id: number
  username: string
  kind: string
  outcome: TraceOutcome
  turns: number
  tool_calls: number
  duration_ms: number
  error: string | null
}

export type AdminTraces = { rows: TraceRow[]; kinds: string[] }

// A release build drops `args` and `result`; the rest is always recorded.
export type TraceCall = {
  name: string
  ms: number
  is_error: boolean
  error_kind: string | null
  args?: string | null
  result?: string | null
}

// A round whose provider call failed carries `error` and no calls.
export type TraceRound = { ms: number; error: string | null; calls?: TraceCall[] }

// `full` is false on a release build, where `opening` and `reply` are absent too.
export type TraceDetail = TraceRow & {
  rounds: TraceRound[]
  opening?: string | null
  reply?: string | null
  full: boolean
}

export type InspectMemory = { id: string; category: string; summary: string; archived: boolean }

export type InspectUser = {
  config_path: string
  config_toml: string
  tasks: TaskNode[]
  conversations: Conversation[]
  memory: InspectMemory[]
  events_today: PlanEvent[]
}

export type SqlResult =
  | { columns: string[]; rows: unknown[][]; truncated: boolean }
  | { changes: number }

export type CalendarKind = 'fixed' | 'busy' | 'note' | 'free'

// `days` is a weekday bitmask, Mon = 1 … Sun = 64; 0 means the entry happens once, on
// `on_date`. `exceptions` are the dates its occurrence is skipped.
export type CalendarEntry = {
  id: number
  external_id: string | null
  title: string
  kind: CalendarKind
  quiet: boolean
  start_time: string
  end_time: string
  days: number
  day_names: string[]
  on_date: string | null
  from_date: string | null
  until_date: string | null
  created_at: string
  updated_at: string
  exceptions: string[]
}

export type CalendarOccurrence = {
  entry_id: number
  title: string
  kind: CalendarKind
  quiet: boolean
  start: string
  end: string
}


export type HistoryKind =
  | 'event_done'
  | 'event_dropped'
  | 'event_moved'
  | 'event_snoozed'
  | 'event_fired'
  | 'task_done'
  | 'checkin'

// `at` is RFC 3339 UTC; `time` is the same instant as HH:MM where the user lives.
export type HistoryRow = {
  at: string
  time: string
  kind: HistoryKind
  label: string
  event_id?: number
  task_id?: number
  conversation_id?: number
}

export type FreeWindow = { start: string; end: string }

// One local day, whole. `history` is empty for a day that has not started, and
// `quiet_now` is only ever set for today.
export type DayView = {
  date: string
  events: PlanEvent[]
  calendar: CalendarOccurrence[]
  free: FreeWindow[]
  quiet_now: string | null
  history: HistoryRow[]
}

export type SessionMode = 'single' | 'pomodoro'

export type SessionPhase = 'work' | 'break'

// The session the user is in, as the server holds it: the clock, the pause, the
// step and the phase are all its own. Every time is RFC 3339 UTC.
export type WorkSession = {
  id: number
  task_id: number | null
  event_id: number | null
  title: string
  planned_min: number | null
  started_at: string
  paused_at: string | null
  paused_ms: number
  mode: SessionMode
  work_min: number | null
  break_min: number | null
  phase: SessionPhase
  phase_started_at: string | null
  phase_paused_ms: number
  round: number
  step_index: number | null
  step_count: number | null
  step_name: string | null
  notes: string
  // The session thread; break reports and Note's own check-ins are in it.
  conversation_id: number | null
}

// What a session is opened with; the mode and the pomodoro lengths are the
// server's to decide from the user's settings.
export type SessionStart = {
  title: string
  task_id?: number
  event_id?: number
  planned_min?: number
  step_index?: number
  step_count?: number
  step_name?: string
  notes?: string
}

// Today's order: the tasks and steps Now works through, first to last.
export type RunOrder = { date: string; task_ids: number[] }

export type QueueReason = 'now' | 'overdue' | 'urgent' | 'due_soon' | 'oldest' | 'scheduled' | 'order'

// `task` is the whole task with its steps; `step` is the one to work on, if any;
// `event_id` is the block a scheduled entry was laid in.
export type QueueEntry = {
  task: TaskNode
  step: Task | null
  planned_min: number | null
  reason: QueueReason
  event_id?: number
}

// How many blocks the close of the day sent to tomorrow.
export type Carried = { moved: number }

export type ShareScope = {
  today: boolean
  tasks: boolean
  categories: string[]
  goals: boolean
  progress: boolean
  details: boolean
  horizon_days: number
  notes: boolean
  messages_per_day: number
}

// What a visitor learns about the link itself; `owner` is a display name.
export type ShareInfo = { owner: string; name: string; expires_at: string; scope: ShareScope; notes: boolean }

export type ShareMessage = { role: 'user' | 'assistant' | 'note'; content: string; created_at: string }
export type Share = {
  id: number
  name: string
  brief: string
  scope: ShareScope
  expires_at: string
  created_at: string
  last_used_at: string | null
  url: string
  messages_today: number
  threads: number
  visitors: number
  distant_visits: number
}
export type NewShare = { name: string; brief: string; scope: ShareScope; expires_at: string }
export type SharePatch = Partial<NewShare>
export type ShareThread = { id: number; created_at: string; updated_at: string; messages: ShareMessage[] }
export type ShareTurn = { thread: number; reply: string; note: boolean }
export type ShareVisit = { city: string | null; country: string | null; km: number | null; distant: boolean; at: string }
