import type {
  Conversation,
  Debrief,
  LogRow,
  Me,
  PlanEvent,
  Settings,
  TalkMessage,
  TalkReply,
  Task,
} from './types'

const WRITABLE_SETTINGS = ['display_name', 'timezone', 'nightly_time', 'template'] as const

type SettingsPatch = Partial<Pick<Settings, (typeof WRITABLE_SETTINGS)[number]>>

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

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    headers: init?.body ? { 'Content-Type': 'application/json' } : undefined,
    ...init,
  })
  if (!res.ok) {
    if (res.status === 401 && path !== '/api/login') onUnauthorized?.()
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
  planToday: () => request<PlanEvent[]>('/api/plan/today'),
  eventAction: (id: number, action: 'done' | 'drop') =>
    request<void>(`/api/events/${id}/${action}`, { method: 'POST' }),
  shift: (id: number, minutes: number) =>
    request<void>(`/api/events/${id}/shift`, { method: 'POST', body: JSON.stringify({ minutes }) }),
  snooze: (id: number, minutes: number) =>
    request<void>(`/api/events/${id}/snooze`, { method: 'POST', body: JSON.stringify({ minutes }) }),
  tasks: () => request<Task[]>('/api/tasks'),
  addTask: (title: string) =>
    request<Task>('/api/tasks', { method: 'POST', body: JSON.stringify({ title }) }),
  patchTask: (id: number, state: Task['state']) =>
    request<Task>(`/api/tasks/${id}`, { method: 'PATCH', body: JSON.stringify({ state }) }),
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
  // The server rejects unknown fields, so only the writable keys actually set go on the wire.
  saveSettings: (patch: SettingsPatch) => {
    const body: SettingsPatch = {}
    for (const key of WRITABLE_SETTINGS) {
      const value = patch[key]
      if (value !== undefined) body[key] = value
    }
    return request<void>('/api/settings', { method: 'PUT', body: JSON.stringify(body) })
  },
  debrief: () => request<Debrief>('/api/debrief'),
  vapidKey: () => request<{ key: string }>('/api/push/vapid_public_key'),
  pushSubscribe: (sub: PushSubscriptionJSON) =>
    request<void>('/api/push/subscribe', { method: 'POST', body: JSON.stringify(sub) }),
  pushUnsubscribe: (endpoint: string) =>
    request<void>('/api/push/unsubscribe', { method: 'POST', body: JSON.stringify({ endpoint }) }),
  adminLog: () => request<LogRow[]>('/api/admin/log?limit=100'),
}
