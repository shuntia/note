import type {
  AdminGate,
  AdminLog,
  AdminStatus,
  AdminTraces,
  AdminUser,
  AlertPatch,
  Allocation,
  CalendarEntry,
  CalendarKind,
  Carried,
  Conversation,
  DayView,
  Debrief,
  FlattenResult,
  Goal,
  GoalState,
  InspectUser,
  Me,
  MemoryFact,
  MemoryHit,
  NewStep,
  PlanEvent,
  Passkey,
  PromptDoc,
  PromptName,
  Review,
  SecurityState,
  SessionStart,
  Settings,
  SettingsSaved,
  NewShare,
  Share,
  ShareInfo,
  ShareMessage,
  SharePatch,
  ShareThread,
  ShareTurn,
  SqlResult,
  TalkMessage,
  TalkReply,
  TelegramLink,
  Task,
  TaskNode,
  TaskNotify,
  TaskState,
  TaskUpdate,
  TaskUrgency,
  Token,
  TokenCreated,
  TotpEnrolment,
  TraceDetail,
  WorkSession,
} from './types'
import type {
  AssertionJSON,
  CreationChallenge,
  RegistrationJSON,
  RequestChallenge,
} from './webauthn'

const WRITABLE_SETTINGS = [
  'display_name',
  'timezone',
  'nightly_time',
  'close_day_time',
  'template',
  'show_arc_between_sessions',
  'counter',
  'nightly_enabled',
  'checkins_enabled',
  'triggers_per_day',
  'pomodoro_enabled',
  'pomodoro_work_min',
  'pomodoro_break_min',
] as const

type SettingsPatch = Partial<Pick<Settings, (typeof WRITABLE_SETTINGS)[number]>>

type NewTaskOpts = {
  duration_min?: number
  parent_id?: number
  is_now?: boolean
  due_at?: string | null
  url?: string
  notify?: TaskNotify
  progress?: number
  category?: string
  goal_id?: number
}

type NewGoal = { title: string; description?: string; due_at?: string | null }

type GoalPatch = {
  title?: string
  description?: string
  state?: GoalState
  due_at?: string | null
}

// A calendar entry recurs on `days` or happens once on `on_date`, never both.
type CalendarFields = {
  title: string
  kind: CalendarKind
  quiet: boolean
  start_time: string
  end_time: string
  days: number
  on_date: string | null
}

type TaskPatch = {
  state?: TaskState
  is_now?: boolean
  notes?: string
  due_at?: string | null
  url?: string
  notify?: TaskNotify
  progress?: number
  category?: string
  urgency?: TaskUrgency
  // null detaches the task from whatever goal it was on.
  goal_id?: number | null
}

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message)
  }
}

let onUnauthorized: (() => void) | null = null

// Lets the shell drop back to the login screen when any request finds the session expired.
export function setOnUnauthorized(fn: () => void) {
  onUnauthorized = fn
}

// Keepalive keeps a request started during `pagehide` alive past the unload; it
// carries a body cap, so a rare oversized payload goes out the ordinary way.
const KEEPALIVE_MAX_BODY = 60_000

// `quiet401` is for admin elevation, where a 401 means the grant is gone rather
// than the session.
type Options = RequestInit & { quiet401?: boolean }

async function request<T>(path: string, init?: Options): Promise<T> {
  const { quiet401, ...rest } = init ?? {}
  const body = rest.body
  const res = await fetch(path, {
    headers: body ? { 'Content-Type': 'application/json' } : undefined,
    keepalive: typeof body === 'string' ? body.length <= KEEPALIVE_MAX_BODY : true,
    ...rest,
  })
  if (!res.ok) {
    if (res.status === 401 && path !== '/api/login' && !quiet401) onUnauthorized?.()
    let message = `Request failed (${res.status})`
    try {
      const body: unknown = await res.json()
      if (
        typeof body === 'object' &&
        body !== null &&
        'error' in body &&
        typeof (body as { error: unknown }).error === 'string'
      ) {
        message = (body as { error: string }).error
      }
    } catch {
      // no JSON body on this error
    }
    throw new ApiError(res.status, message)
  }
  const text = await res.text()
  return (text ? JSON.parse(text) : undefined) as T
}

export const api = {
  me: () => request<Me>('/api/me'),
  login: (username: string, password: string) =>
    request<void>('/api/login', { method: 'POST', body: JSON.stringify({ username, password }) }),
  logout: () => request<void>('/api/logout', { method: 'POST' }),
  day: (date: string) => request<DayView>(`/api/day/${date}`),
  planRange: (from: string, to: string) =>
    request<{ days: Record<string, PlanEvent[]> }>(
      `/api/plan/range?${new URLSearchParams({ from, to }).toString()}`,
    ),
  allocate: (date: string) => request<Allocation>(`/api/plan/${date}/allocate`, { method: 'POST' }),
  carry: (date: string) => request<Carried>(`/api/plan/${date}/carry`, { method: 'POST' }),
  eventAction: (id: number, action: 'done' | 'drop') =>
    request<void>(`/api/events/${id}/${action}`, { method: 'POST' }),
  shift: (id: number, minutes: number) =>
    request<void>(`/api/events/${id}/shift`, { method: 'POST', body: JSON.stringify({ minutes }) }),
  snooze: (id: number, minutes: number) =>
    request<void>(`/api/events/${id}/snooze`, { method: 'POST', body: JSON.stringify({ minutes }) }),
  setEventAlert: (id: number, alert: boolean) =>
    request<void>(`/api/events/${id}/alert`, { method: 'POST', body: JSON.stringify({ alert }) }),
  moveTomorrow: (id: number) =>
    request<{ event_id: number; date: string }>(`/api/events/${id}/move_tomorrow`, {
      method: 'POST',
    }),
  tasks: () => request<TaskNode[]>('/api/tasks'),
  addTask: (title: string, opts?: NewTaskOpts) =>
    request<Task>('/api/tasks', { method: 'POST', body: JSON.stringify({ title, ...opts }) }),
  // The server rejects unknown fields, and undefined keys drop out of the body,
  // so a patch carries exactly the fields the caller set.
  patchTask: (id: number, patch: TaskPatch) =>
    request<TaskUpdate>(`/api/tasks/${id}`, { method: 'PATCH', body: JSON.stringify(patch) }),
  splitTask: (id: number, steps: NewStep[]) =>
    request<TaskNode>(`/api/tasks/${id}/split`, {
      method: 'POST',
      body: JSON.stringify({ steps }),
    }),
  flattenTask: (id: number) =>
    request<FlattenResult>(`/api/tasks/${id}/flatten`, { method: 'POST' }),
  // Every state, so a caller that only wants the open ones says so itself.
  goals: () => request<Goal[]>('/api/goals'),
  addGoal: (fields: NewGoal) =>
    request<Goal>('/api/goals', { method: 'POST', body: JSON.stringify(fields) }),
  patchGoal: (id: number, patch: GoalPatch) =>
    request<Goal>(`/api/goals/${id}`, { method: 'PATCH', body: JSON.stringify(patch) }),
  deleteGoal: (id: number) => request<void>(`/api/goals/${id}`, { method: 'DELETE' }),
  // 401 when `current` is wrong, 422 when the new one is too short.
  changePassword: (current: string, next: string) =>
    request<void>('/api/password', {
      method: 'POST',
      body: JSON.stringify({ current, new: next }),
      quiet401: true,
    }),
  tokens: () => request<Token[]>('/api/tokens'),
  createToken: (name: string) =>
    request<TokenCreated>('/api/tokens', { method: 'POST', body: JSON.stringify({ name }) }),
  revokeToken: (id: number) => request<void>(`/api/tokens/${id}`, { method: 'DELETE' }),
  conversations: () => request<Conversation[]>('/api/conversations'),
  renameConversation: (id: number, title: string) =>
    request<void>(`/api/conversations/${id}`, {
      method: 'PATCH',
      body: JSON.stringify({ title }),
    }),
  deleteConversation: (id: number) =>
    request<void>(`/api/conversations/${id}`, { method: 'DELETE' }),
  conversationMessages: (id: number) => request<TalkMessage[]>(`/api/conversations/${id}/messages`),
  talk: (message: string, conversationId?: number) =>
    request<TalkReply>('/api/talk', {
      method: 'POST',
      body: JSON.stringify(
        conversationId === undefined ? { message } : { message, conversation_id: conversationId },
      ),
    }),
  settings: () => request<Settings>('/api/settings'),
  // The server rejects unknown fields, so only the writable keys actually set go on the
  // wire. Bell toggles apply to the template the request leaves selected.
  saveSettings: (patch: SettingsPatch, alerts?: AlertPatch[]) => {
    const body: SettingsPatch & { alerts?: AlertPatch[] } = {}
    for (const key of WRITABLE_SETTINGS) {
      const value = patch[key]
      if (value !== undefined) Object.assign(body, { [key]: value })
    }
    if (alerts?.length) body.alerts = alerts
    return request<SettingsSaved>('/api/settings', { method: 'PUT', body: JSON.stringify(body) })
  },
  promptGet: (name: PromptName) => request<PromptDoc>(`/api/prompts/${name}`),
  promptPut: (name: PromptName, content: string) =>
    request<PromptDoc>(`/api/prompts/${name}`, {
      method: 'PUT',
      body: JSON.stringify({ content }),
    }),
  // Dropping the override; the reply carries the shipped default that takes over.
  promptReset: (name: PromptName) =>
    request<PromptDoc>(`/api/prompts/${name}`, { method: 'DELETE' }),
  // A search covers every category, so `q` and `category` never travel together.
  memoryList: (params: { category?: string; q?: string }) => {
    const search = new URLSearchParams()
    if (params.q) search.set('q', params.q)
    else if (params.category) search.set('category', params.category)
    const query = search.toString()
    return request<{ items: MemoryHit[] }>(`/api/memory${query ? `?${query}` : ''}`)
  },
  memoryRead: (id: string) => request<MemoryFact>(`/api/memory/${encodeURIComponent(id)}`),
  debrief: () => request<Debrief>('/api/debrief'),
  review: () => request<Review>('/api/review'),
  vapidKey: () => request<{ key: string }>('/api/push/vapid_public_key'),
  pushSubscribe: (sub: PushSubscriptionJSON) =>
    request<void>('/api/push/subscribe', { method: 'POST', body: JSON.stringify(sub) }),
  pushUnsubscribe: (endpoint: string) =>
    request<void>('/api/push/unsubscribe', { method: 'POST', body: JSON.stringify({ endpoint }) }),
  // A new code replaces whatever the account was last given.
  telegramLink: () => request<TelegramLink>('/api/telegram/link', { method: 'POST' }),
  telegramUnlink: () => request<void>('/api/telegram/link', { method: 'DELETE' }),
  notifyTest: () => request<{ via: string }>('/api/notify/test', { method: 'POST' }),
  openWorkSession: () => request<WorkSession | null>('/api/sessions/open'),
  // Opening one ends whatever was still running, farewell and all.
  startWorkSession: (fields: SessionStart) =>
    request<WorkSession>('/api/sessions', { method: 'POST', body: JSON.stringify(fields) }),
  pauseWorkSession: (id: number) =>
    request<WorkSession>(`/api/sessions/${id}/pause`, { method: 'POST' }),
  resumeWorkSession: (id: number) =>
    request<WorkSession>(`/api/sessions/${id}/resume`, { method: 'POST' }),
  stepWorkSession: (id: number, step: { step_index: number; step_name: string }) =>
    request<WorkSession>(`/api/sessions/${id}/step`, {
      method: 'POST',
      body: JSON.stringify(step),
    }),
  skipBreak: (id: number) =>
    request<WorkSession>(`/api/sessions/${id}/skip_break`, { method: 'POST' }),
  endWorkSession: (id: number, outcome: 'done' | 'stopped') =>
    request<{ ended: number | null }>(`/api/sessions/${id}/end`, {
      method: 'POST',
      body: JSON.stringify({ outcome }),
    }),
  calendar: () => request<{ entries: CalendarEntry[] }>('/api/calendar'),
  addCalendarEntry: (fields: CalendarFields) =>
    request<CalendarEntry>('/api/calendar', { method: 'POST', body: JSON.stringify(fields) }),
  // The server rejects unknown fields and keeps whatever the patch leaves out; an empty
  // string is how a date is cleared.
  patchCalendarEntry: (id: number, patch: Partial<CalendarFields>) =>
    request<CalendarEntry>(`/api/calendar/${id}`, {
      method: 'PATCH',
      body: JSON.stringify(patch),
    }),
  deleteCalendarEntry: (id: number) =>
    request<void>(`/api/calendar/${id}`, { method: 'DELETE' }),
  skipCalendarDate: (id: number, date: string) =>
    request<void>(`/api/calendar/${id}/skip`, { method: 'POST', body: JSON.stringify({ date }) }),
  unskipCalendarDate: (id: number, date: string) =>
    request<void>(`/api/calendar/${id}/skip/${date}`, { method: 'DELETE' }),
  shares: () => request<Share[]>('/api/shares'),
  createShare: (body: NewShare) => request<Share>('/api/shares', { method: 'POST', body: JSON.stringify(body) }),
  updateShare: (id: number, body: SharePatch) =>
    request<Share>(`/api/shares/${id}`, { method: 'PATCH', body: JSON.stringify(body) }),
  revokeShare: (id: number) => request<void>(`/api/shares/${id}`, { method: 'DELETE' }),
  shareThreads: (id: number) => request<ShareThread[]>(`/api/shares/${id}/threads`),
  share: {
    info: (token: string) => request<ShareInfo>(`/api/share/${token}`, { quiet401: true }),
    messages: (token: string) => request<ShareMessage[]>(`/api/share/${token}/messages`, { quiet401: true }),
    send: (token: string, message: string) =>
      request<ShareTurn>(`/api/share/${token}/messages`, { method: 'POST', body: JSON.stringify({ message }), quiet401: true }),
  },
}

const SECURITY = '/api/security'

// Every mutating call carries the account password: adding or dropping a second
// factor asks for it again, whatever the session already proved.
export const security = {
  state: () => request<SecurityState>(SECURITY),
  passkeyChallenge: (password: string) =>
    request<CreationChallenge>(`${SECURITY}/passkeys/challenge`, {
      method: 'POST',
      body: JSON.stringify({ password }),
    }),
  addPasskey: (name: string, credential: RegistrationJSON) =>
    request<Passkey>(`${SECURITY}/passkeys`, {
      method: 'POST',
      body: JSON.stringify({ name, credential }),
    }),
  renamePasskey: (id: number, name: string) =>
    request<Passkey>(`${SECURITY}/passkeys/${id}`, {
      method: 'PATCH',
      body: JSON.stringify({ name }),
    }),
  removePasskey: (id: number, password: string) =>
    request<void>(`${SECURITY}/passkeys/${id}`, {
      method: 'DELETE',
      body: JSON.stringify({ password }),
    }),
  totpStart: (password: string) =>
    request<TotpEnrolment>(`${SECURITY}/totp/start`, {
      method: 'POST',
      body: JSON.stringify({ password }),
    }),
  totpConfirm: (code: string) =>
    request<void>(`${SECURITY}/totp/confirm`, { method: 'POST', body: JSON.stringify({ code }) }),
  totpRemove: (password: string) =>
    request<void>(`${SECURITY}/totp`, { method: 'DELETE', body: JSON.stringify({ password }) }),
}

const ADMIN = '/api/admin'

// Every grant-protected route answers 401 when elevation has expired; the panel
// handles that itself rather than dropping the whole session.
const guarded = <T,>(path: string, init?: RequestInit) =>
  request<T>(`${ADMIN}${path}`, { ...init, quiet401: true })

export const admin = {
  gate: () => request<AdminGate>(`${ADMIN}/gate`),
  elevate: (password: string, code?: string) =>
    request<void>(`${ADMIN}/elevate`, {
      method: 'POST',
      body: JSON.stringify(code ? { password, code } : { password }),
      quiet401: true,
    }),
  elevateChallenge: () =>
    request<RequestChallenge>(`${ADMIN}/elevate/challenge`, { method: 'POST', quiet401: true }),
  elevateWithPasskey: (password: string, assertion: AssertionJSON) =>
    request<void>(`${ADMIN}/elevate`, {
      method: 'POST',
      body: JSON.stringify({ password, assertion }),
      quiet401: true,
    }),
  drop: () => request<void>(`${ADMIN}/drop`, { method: 'POST' }),
  status: () => guarded<AdminStatus>('/status'),
  users: () => guarded<AdminUser[]>('/users'),
  createUser: (username: string, password: string, admin: boolean) =>
    guarded<{ id: number }>('/users', {
      method: 'POST',
      body: JSON.stringify({ username, password, admin }),
    }),
  patchUser: (
    id: number,
    patch: { role?: 'admin' | 'member'; disabled?: boolean; password?: string },
  ) => guarded<void>(`/users/${id}`, { method: 'PATCH', body: JSON.stringify(patch) }),
  revokeSessions: (id: number) =>
    guarded<{ revoked: number }>(`/users/${id}/revoke_sessions`, { method: 'POST' }),
  log: (params: { limit?: number; kind?: string; before_id?: number }) => {
    const search = new URLSearchParams()
    if (params.limit !== undefined) search.set('limit', String(params.limit))
    if (params.kind) search.set('kind', params.kind)
    if (params.before_id !== undefined) search.set('before_id', String(params.before_id))
    const query = search.toString()
    return guarded<AdminLog>(`/log${query ? `?${query}` : ''}`)
  },
  traces: (params: { limit?: number; kind?: string; outcome?: string; before_id?: number }) => {
    const search = new URLSearchParams()
    if (params.limit !== undefined) search.set('limit', String(params.limit))
    if (params.kind) search.set('kind', params.kind)
    if (params.outcome) search.set('outcome', params.outcome)
    if (params.before_id !== undefined) search.set('before_id', String(params.before_id))
    const query = search.toString()
    return guarded<AdminTraces>(`/traces${query ? `?${query}` : ''}`)
  },
  trace: (id: number) => guarded<TraceDetail>(`/traces/${id}`),
  inspectUser: (id: number) => guarded<InspectUser>(`/inspect/users/${id}`),
  putConfig: (id: number, toml: string) =>
    guarded<void>(`/inspect/users/${id}/config`, {
      method: 'PUT',
      body: JSON.stringify({ toml }),
    }),
  conversationMessages: (id: number, cid: number) =>
    guarded<TalkMessage[]>(`/inspect/users/${id}/conversations/${cid}`),
  memoryGet: (id: number, mid: string) =>
    guarded<{ content: string }>(`/inspect/users/${id}/memory/${encodeURIComponent(mid)}`),
  memoryPut: (id: number, mid: string, content: string) =>
    guarded<void>(`/inspect/users/${id}/memory/${encodeURIComponent(mid)}`, {
      method: 'PUT',
      body: JSON.stringify({ content }),
    }),
  sql: (sql: string) =>
    guarded<SqlResult>('/inspect/sql', { method: 'POST', body: JSON.stringify({ sql }) }),
}
