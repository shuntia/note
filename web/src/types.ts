export type Me = { username: string; admin: boolean }

export type PlanEvent = {
  id: number
  kind: string
  wall_time: string
  status: 'pending' | 'fired' | 'snoozed' | 'done' | 'dropped'
  flexibility: 'fixed' | 'slide' | 'drop'
  slide_window_min: number
  channel: string
}

export type Task = {
  id: number
  title: string
  description: string
  state: 'open' | 'in_progress' | 'done' | 'dropped'
  source: string
  notes: string
}

export type Debrief = { date: string; content: string }

export type LogRow = { ts: string; user_id: number | null; kind: string; detail: string }
