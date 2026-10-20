import type { Effort } from './models'
import type { WritableSetting } from './api'
import type { AgentFrame } from './ws'

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
export type EventOrigin = 'template' | 'agent' | 'auto' | 'user' | 'idle' | 'lay_day' | 'await_reply'

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
  // 'derived': a parent with sized children reads the sum of its live children.
  duration_source: 'user' | 'agent' | 'none' | 'derived'
  parent_id: number | null
  root_id: number
  // 0 for a root, at most 4 below it.
  depth: number
  // 0-based order among its siblings.
  position: number
  is_now: boolean
  updated_at: string
  // The node's own deadline, RFC 3339 UTC like updated_at.
  due_at: string | null
  // YYYY-MM-DD when it is due some time that day rather than at a set moment;
  // due_at is then the end of it.
  due_on: string | null
  // The own due, else the nearest ancestor's; `due_from` is the node it comes from.
  effective_due_at: string | null
  effective_due_on: string | null
  due_from: number | null
  external_id: string | null
  url: string
  notify: TaskNotify
  // Free text, one per tree; every node reports its root's.
  category: string
  urgency: TaskUrgency
  // The highest of its own urgency and its ancestors'.
  effective_urgency: TaskUrgency
  // The effective due falls inside the next two days or is already past.
  pressing: boolean
  // The root's goal, reported on every node of the tree.
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

// Live children in `position` order, recursively.
export type TaskNode = Task & { children: TaskNode[] }

export type GoalState = 'open' | 'done' | 'dropped'

// `tasks` counts everything hanging from the goal that was not dropped;
// `next_*` name the unfinished one whose deadline comes first.
export type Goal = {
  id: number
  title: string
  description: string
  due_at: string | null
  due_on: string | null
  state: GoalState
  created_at: string
  updated_at: string
  tasks: number
  done_tasks: number
  next_task_id: number | null
  next_task_title: string | null
  next_due_at: string | null
  next_due_on: string | null
}

export type Token = {
  id: number
  name: string
  created_at: string
  last_used_at: string | null
}

// The secret is present only in the create response.
export type TokenCreated = Token & { token: string }

// A change the server made beyond what was asked: a due clamped to fit ancestor
// `by`, or a relative that left Now because `by` took its place.
export type Adjusted = {
  id: number
  title: string
  field: 'due' | 'duration_min' | 'is_now' | 'category' | 'goal_id' | 'depth' | 'state'
  to?: unknown
  why: string
  by?: number
  by_title?: string
}

// `parent` arrives when finishing or reopening cascaded to it; `demoted_from_now`
// when the write pushed other tasks out of Now, newest first.
export type TaskUpdate = Task & { parent?: Task; demoted_from_now?: number[]; adjusted?: Adjusted[] }

export type TaskCreated = Task & { children: TaskNode[]; adjusted?: Adjusted[] }

export type NewStep = { title: string; duration_min: number }

export type NewTask = {
  title: string
  duration_min?: number
  due_at?: string
  due_on?: string
  children?: NewTask[]
}

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
  // Null follows the user's default from Settings.
  model: string | null
  effort: Effort | null
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

// The user's turn in flight and every agent frame it has sent.
export type LiveTurn = { conversation_id: number; frames: AgentFrame[] }

export type TalkReply = {
  conversation_id: number
  reply: string
  steps: TalkStep[]
  reasoning: string
  thought_ms: number
}

// A turn still going when the request stopped waiting; it ends on the socket.
// `turn` is the id of the message row it answers.
export type TalkPending = { conversation_id: number; turn: number; pending: true }

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
  // Until when Note may call, HH:MM; '' runs calls to midnight.
  calls_until: string
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
  // How many check-ins Note may start on its own in a day; -1 is no limit.
  triggers_per_day: number
  pomodoro_enabled: boolean
  pomodoro_work_min: number
  pomodoro_break_min: number
  // Gates the pomodoro phase messages and the "Time's up." at a session's planned end.
  session_end_notify: boolean
  // Whether a session run far past its plan wakes Note to answer it in its own words.
  overrun_wakes: boolean
  // Whether Chat lists the read-only Triggers thread.
  show_triggers: boolean
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
  // Null follows the server's own.
  model: string | null
  effort: Effort | null
}

export type Language = '' | 'en' | 'ja'

export type RingFor = 'allowed' | 'never'

export type VoiceChoice = { id: string; label: string; backend?: string; slow?: boolean; credit?: string }

export type AlertPatch = { index: number; alert: boolean }

export type SettingsSaved = Pick<Settings, WritableSetting> & { schedule: ScheduleRow[] }

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

export type AdminGate = {
  elevated: boolean
  expires_at?: string
  second_factor: 'passkey' | 'totp' | 'none'
  methods: { passkey: boolean; totp: boolean }
  require_second_factor: boolean
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

export type TraceOutcome = 'ok' | 'max_turns' | 'stopped' | 'error'

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

// Something Note leans on that is failing for this user, and since when; the
// server's `outage::Outage` codes.
export type Outage = { code: FailureReason | 'daily_cap' | 'matrix_down' | 'voice_down'; since: string }

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
  // The leaf being worked under `task_id`; `leaf_path` runs from task_id's
  // title down to it. Index and count place it among the live leaves under
  // task_id, depth first, and are null when the leaf is task_id itself.
  leaf_id: number | null
  leaf_title: string | null
  leaf_path: string[]
  leaf_index: number | null
  leaf_count: number | null
  // Mirrors of leaf_index, leaf_count and leaf_title.
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
  // Absent: the server picks the first open leaf under task_id.
  leaf_id?: number
  notes?: string
}

// Today's order: the tasks Now works through, first to last.
export type RunOrder = { date: string; task_ids: number[] }

export type QueueReason = 'now' | 'overdue' | 'urgent' | 'due_soon' | 'oldest' | 'scheduled' | 'order'

// `task` is the whole tree; `leaf` is the first open leaf it would begin with,
// null when the root is itself the leaf; `path` runs from the root's title to
// the leaf's. `event_id` is the block a scheduled entry was laid in.
export type QueueEntry = {
  task: TaskNode
  leaf: Task | null
  path: string[]
  // Equal to `leaf`.
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

// One of Note's trigger points as Chat's read-only Triggers thread shows it. `date`
// and `at` are where the user lives; `text` is what Note said, or why it stayed
// quiet or was called off.
export type TriggerLogEntry = {
  event_id: number
  date: string
  at: string
  status: 'pending' | 'snoozed' | 'fired' | 'done' | 'dropped'
  prompt: string
  origin: string
  outcome: 'said' | 'rang' | 'quiet' | 'cancelled' | 'replaced' | null
  text: string | null
}
