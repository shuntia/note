import { api, ApiError } from './api'
import { t } from './i18n'
import type { ModelPick } from './models'
import type { TalkMessage, TalkReply } from './types'
import { onAgentFrame, type TalkDoneFrame } from './ws'

// How often a turn left pending is looked in on, in case its socket frame never comes.
const POLL_MS = 10_000

// One Talk turn. A turn the server stopped waiting on is followed to its end on the
// socket, so a long job reads the same as a short one: a reply, or an ApiError. An aborted
// `signal` rejects at once, waiting or not.
export async function talkTurn(
  message: string,
  conversationId?: number,
  opensWith?: ModelPick,
  signal?: AbortSignal,
): Promise<TalkReply> {
  const ended = new Map<number, TalkDoneFrame>()
  let heard: (() => void) | null = null
  const off = onAgentFrame((frame) => {
    if (frame.type !== 'talk_done') return
    ended.set(frame.turn, frame)
    heard?.()
  })
  const wake = () => heard?.()
  signal?.addEventListener('abort', wake)
  try {
    const answer = await api.talk(message, conversationId, opensWith, signal)
    if (!('pending' in answer)) return answer
    const { conversation_id: id, turn } = answer
    for (;;) {
      signal?.throwIfAborted()
      const done = ended.get(turn)
      if (done?.failure) throw new ApiError(502, done.failure.error, done.failure.reason)
      if (done) break
      await new Promise<void>((resolve) => {
        heard = resolve
        setTimeout(resolve, POLL_MS)
      })
      heard = null
      signal?.throwIfAborted()
      if (!ended.has(turn)) {
        const live = await api.talkLive().catch(() => undefined)
        if (live === null || (live && live.conversation_id !== id)) break
      }
    }
    return replyOf(id, turn, await api.conversationMessages(id))
  } finally {
    signal?.removeEventListener('abort', wake)
    off()
  }
}

// The turn's reply as the thread holds it: the calls and the answer after the row it answers.
function replyOf(id: number, turn: number, rows: TalkMessage[]): TalkReply {
  const at = rows.findIndex((r) => r.id === turn)
  const after = at < 0 ? [] : rows.slice(at + 1)
  const end = after.findIndex((r) => r.role === 'assistant')
  if (end < 0) throw new ApiError(502, t('talk.unreachable'), 'internal')
  const answer = after[end]
  return {
    conversation_id: id,
    reply: answer.content,
    reasoning: answer.reasoning ?? '',
    thought_ms: answer.thought_ms ?? 0,
    steps: after
      .slice(0, end)
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
