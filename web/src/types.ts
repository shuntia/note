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

export type Conversation = { id: number; title: string; updated_at: string }

export type TalkStep = { name: string; args: string; result: string; is_error: boolean }

export type TalkMessage = {
  id: number
  role: 'user' | 'assistant' | 'tool'
  content: string
  tool_name: string | null
  tool_args: string | null
  is_error: boolean
  created_at: string
}

export type TalkReply = { conversation_id: number; reply: string; steps: TalkStep[] }

export type Settings = {
  display_name: string
  timezone: string
  nightly_time: string
  template: string
  templates: string[]
  timezones: string[]
}

export type PromptName = 'persona' | 'planning'

// `content` is the effective prompt; `custom` marks it as the user's own override.
export type PromptDoc = { name: PromptName; content: string; custom: boolean }

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

export type LogRow = { ts: string; user_id: number | null; kind: string; detail: string }
