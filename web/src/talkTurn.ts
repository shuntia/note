import { api, ApiError } from './api'
import { t } from './i18n'
import type { FailureReason, TalkMessage, TalkReply } from './types'
import { onAgentFrame, type TalkDoneFrame } from './ws'

// How often a turn left pending is looked in on, in case its socket frame never comes.
const POLL_MS = 10_000

// One Talk turn. A turn the server stopped waiting on is followed to its end on the
// socket, so a long job reads the same as a short one: a reply, or an ApiError.
export async function talkTurn(message: string, conversationId?: number): Promise<TalkReply> {
  const ended = new Map<number, TalkDoneFrame>()
  let heard: (() => void) | null = null
  const off = onAgentFrame((frame) => {
    if (frame.type !== 'talk_done') return
    ended.set(frame.conversation_id, frame)
    heard?.()
  })
  try {
    const answer = await api.talk(message, conversationId)
    if (!('pending' in answer)) return answer
    const id = answer.conversation_id
    for (;;) {
      const done = ended.get(id)
      if (done?.failure) throw new ApiError(502, done.failure.error, done.failure.reason)
      if (done) break
      await new Promise<void>((resolve) => {
        heard = resolve
        window.setTimeout(resolve, POLL_MS)
      })
      heard = null
      if (!ended.has(id)) {
        const live = await api.talkLive().catch(() => undefined)
        if (live === null || (live && live.conversation_id !== id)) break
      }
    }
    return replyOf(id, await api.conversationMessages(id))
  } finally {
    off()
  }
}

// The turn's reply as the thread holds it: the calls and the answer after its last user row.
function replyOf(id: number, rows: TalkMessage[]): TalkReply {
  const after = rows.slice(rows.map((r) => r.role).lastIndexOf('user') + 1)
  const answer = after.find((r) => r.role === 'assistant')
  if (!answer) throw new ApiError(502, t('talk.unreachable'), 'internal' satisfies FailureReason)
  return {
    conversation_id: id,
    reply: answer.content,
    reasoning: answer.reasoning ?? '',
    thought_ms: answer.thought_ms ?? 0,
    steps: after
      .filter((r) => r.role === 'tool')
      .map((r) => ({
        name: r.tool_name ?? 'tool',
        args: r.tool_args ?? '',
        result: r.content,
        is_error: r.is_error,
        thinking: r.reasoning,
      })),
  }
}
