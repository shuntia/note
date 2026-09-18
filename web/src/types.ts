export type Me = { username: string; admin: boolean }

export type MovedTo = { event_id: number; date: string; wall_time: string; kind: string }

// Where the event came from: the template, the agent, the allocator, or a user edit.
export type EventOrigin = 'template' | 'agent' | 'auto' | 'user'

// The task a block holds, carried so the block can offer the task's own actions.
// `title` is always the top-level task; `step` names the one this block holds.
export type TaskRef = {
  id: number
  title: string
  state: TaskState
  step: string | null
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
}

// A top-level task with its steps; the list never nests deeper than this.
export type TaskNode = Task & { children: Task[] }

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

export type Debrief = { date: string; content: string }

// Where the user last spoke to the thread from; Note answers there.
export type ConversationVia = 'web' | 'telegram'

export type Conversation = {
  id: number
  title: string
  updated_at: string
  summary: string | null
  via: ConversationVia
}

export type TalkStep = { name: string; args: string; result: string; is_error: boolean }

// `reasoning` and `thought_ms` are set on the assistant row only, and are null
// on rows written before the transcript kept them.
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
  nightly_time: string
  // When the close-the-day card appears; '' where the user turned it off.
  close_day_time: string
  template: string
  templates: string[]
  timezones: string[]
  schedule: ScheduleRow[]
  show_arc_between_sessions: boolean
  counter: 'remaining' | 'elapsed'
  category: 'member' | 'test'
  nightly_enabled: boolean
  checkins_enabled: boolean
  telegram_enabled: boolean
  telegram_linked: boolean
  // The bot a link invites the user to; '' where no bot is configured.
  telegram_bot: string
  // How many check-ins Note may start on its own in a day.
  triggers_per_day: number
  pomodoro_enabled: boolean
  pomodoro_work_min: number
  pomodoro_break_min: number
}

// A live code and the deep link that carries it to the bot.
export type TelegramLink = { code: string; bot: string; url: string }

export type AlertPatch = { index: number; alert: boolean }

export type SettingsSaved = Pick<
  Settings,
  | 'display_name'
  | 'timezone'
  | 'nightly_time'
  | 'close_day_time'
  | 'template'
  | 'show_arc_between_sessions'
  | 'counter'
  | 'nightly_enabled'
  | 'checkins_enabled'
  | 'triggers_per_day'
  | 'pomodoro_enabled'
  | 'pomodoro_work_min'
  | 'pomodoro_break_min'
> & { schedule: ScheduleRow[] }

export type PromptName = 'persona' | 'planning'

// `content` is the effective prompt; `custom` marks it as the user's own override.
export type PromptDoc = { name: PromptName; content: string; custom: boolean }

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

// How many blocks the close of the day sent to tomorrow.
export type Carried = { moved: number }

// What the allocator laid down, and how many waiting blocks it replaced.
export type Allocation = {
  plan_date: string
  placed: { event_id: number; task_id: number; start: string; end: string }[]
  cleared: number
}
