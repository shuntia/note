import type { Debrief, LogRow, Me, PlanEvent, Task } from './types'

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message)
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    headers: init?.body ? { 'Content-Type': 'application/json' } : undefined,
    ...init,
  })
  if (!res.ok) {
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
  talk: (message: string) =>
    request<{ reply: string }>('/api/talk', { method: 'POST', body: JSON.stringify({ message }) }),
  debrief: () => request<Debrief>('/api/debrief'),
  vapidKey: () => request<{ key: string }>('/api/push/vapid_public_key'),
  pushSubscribe: (sub: PushSubscriptionJSON) =>
    request<void>('/api/push/subscribe', { method: 'POST', body: JSON.stringify(sub) }),
  pushUnsubscribe: (endpoint: string) =>
    request<void>('/api/push/unsubscribe', { method: 'POST', body: JSON.stringify({ endpoint }) }),
  adminLog: () => request<LogRow[]>('/api/admin/log?limit=100'),
  adminCreateUser: (username: string, password: string, admin: boolean) =>
    request<void>('/api/admin/users', {
      method: 'POST',
      body: JSON.stringify({ username, password, admin }),
    }),
}
