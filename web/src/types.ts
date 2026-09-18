export type Me = { username: string; admin: boolean }

export type MovedTo = { event_id: number; date: string; wall_time: string; kind: string }

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
  // present only when the agent named the event this one moved to
  moved_to?: MovedTo
}

export type TaskState = 'open' | 'in_progress' | 'done' | 'dropped'

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

export type Conversation = {
  id: number
  title: string
  updated_at: string
  summary: string | null
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
  template: string
  templates: string[]
  timezones: string[]
  schedule: ScheduleRow[]
  show_arc_between_sessions: boolean
  counter: 'remaining' | 'elapsed'
  category: 'member' | 'test'
  nightly_enabled: boolean
  checkins_enabled: boolean
  // The topic in use: the user's own where they set one, else the server default.
  ntfy_topic: string
  ntfy_enabled: boolean
  // E.164, or '' when the user has given no number.
  phone_number: string
  calls_enabled: boolean
  voice_enabled: boolean
}

export type AlertPatch = { index: number; alert: boolean }

export type SettingsSaved = Pick<
  Settings,
  | 'display_name'
  | 'timezone'
  | 'nightly_time'
  | 'template'
  | 'show_arc_between_sessions'
  | 'counter'
  | 'nightly_enabled'
  | 'checkins_enabled'
  | 'ntfy_topic'
  | 'phone_number'
  | 'calls_enabled'
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

export type CalendarKind = 'fixed' | 'busy' | 'note'

// `days` is a weekday bitmask, Mon = 1 … Sun = 64; 0 means the entry happens once, on
// `on_date`. `exceptions` are the dates its occurrence is skipped.
export type CalendarEntry = {
  id: number
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

// `quiet_now` is the HH:MM a running quiet window ends, and is only ever set for today.
export type CalendarDay = {
  date: string
  occurrences: CalendarOccurrence[]
  quiet_now: string | null
}
