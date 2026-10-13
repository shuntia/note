import { t } from './i18n'
import type { Task, TaskUrgency } from './types'

const URGENCY_RANK: Record<TaskUrgency, number> = { high: 0, normal: 2, low: 3 }

// 0 high, 1 pressing, 2 normal, 3 low: the order Later reads by default.
export function urgencyRank(task: Pick<Task, 'urgency' | 'pressing'>): number {
  if (task.urgency === 'high') return 0
  if (task.pressing) return 1
  return URGENCY_RANK[task.urgency]
}

// The word sits in sun-ink after the title; overdue keeps rose through `Due`.
export function Urgent({ task }: { task: Pick<Task, 'urgency' | 'pressing' | 'due_at'> }) {
  const overdue = task.due_at !== null && new Date(task.due_at).getTime() < Date.now()
  if (task.urgency !== 'high' && !(task.pressing && !overdue)) return null
  return <span className="meta sun">{t('tasks.urgent')}</span>
}
