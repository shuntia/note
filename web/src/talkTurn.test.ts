import { afterEach, expect, test, vi } from 'vitest'
import { ApiError } from './api'
import type { TalkDoneFrame } from './ws'

const listeners = new Set<(frame: TalkDoneFrame) => void>()
vi.mock('./ws', () => ({
  onAgentFrame: (fn: (frame: TalkDoneFrame) => void) => {
    listeners.add(fn)
    return () => listeners.delete(fn)
  },
}))
const { talkTurn } = await import('./talkTurn')

afterEach(() => {
  vi.unstubAllGlobals()
  listeners.clear()
})

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status })
const row = (id: number, role: string, content: string, extra = {}) => ({
  id,
  role,
  content,
  tool_name: null,
  tool_args: null,
  is_error: false,
  created_at: '',
  reasoning: null,
  thought_ms: null,
  ...extra,
})

test('a pending turn ends on the socket and reads its reply from the thread', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string) => {
      if (path === '/api/talk') {
        queueMicrotask(() => listeners.forEach((fn) => fn({ type: 'talk_done', conversation_id: 7, turn: 3 })))
        return json({ conversation_id: 7, turn: 3, pending: true }, 202)
      }
      return json([
        row(1, 'user', 'earlier'),
        row(2, 'assistant', 'old reply'),
        row(3, 'user', 'a long job'),
        row(4, 'tool', '{}', { tool_name: 'task_create', tool_args: '{"title":"x"}', reasoning: 'first' }),
        row(5, 'assistant', 'all done', { reasoning: 'wrap up', thought_ms: 1200 }),
      ])
    }),
  )
  expect(await talkTurn('a long job')).toEqual({
    conversation_id: 7,
    reply: 'all done',
    reasoning: 'wrap up',
    thought_ms: 1200,
    steps: [{ name: 'task_create', args: '{"title":"x"}', result: '{}', is_error: false, thinking: 'first' }],
  })
})

test('a pending turn that failed throws what the socket said', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => {
      queueMicrotask(() =>
        listeners.forEach((fn) =>
          fn({ type: 'talk_done', conversation_id: 7, turn: 3, failure: { reason: 'provider_down', error: 'down' } }),
        ),
      )
      return json({ conversation_id: 7, turn: 3, pending: true }, 202)
    }),
  )
  const err: unknown = await talkTurn('a long job').catch((e: unknown) => e)
  expect(err).toBeInstanceOf(ApiError)
  expect(err).toMatchObject({ status: 502, message: 'down', reason: 'provider_down' })
})

test('a turn taken back is a failure, never the reply before it', async () => {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string) => {
      if (path === '/api/talk') {
        queueMicrotask(() => listeners.forEach((fn) => fn({ type: 'talk_done', conversation_id: 7, turn: 3 })))
        return json({ conversation_id: 7, turn: 3, pending: true }, 202)
      }
      return json([row(1, 'user', 'earlier'), row(2, 'assistant', 'old reply')])
    }),
  )
  const err: unknown = await talkTurn('a long job').catch((e: unknown) => e)
  expect(err).toBeInstanceOf(ApiError)
  expect(err).toMatchObject({ reason: 'internal' })
})

test('the end of another turn in the same thread is not this one', async () => {
  let release = () => {}
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string) => {
      if (path === '/api/talk') {
        queueMicrotask(() => listeners.forEach((fn) => fn({ type: 'talk_done', conversation_id: 7, turn: 1 })))
        release = () => listeners.forEach((fn) => fn({ type: 'talk_done', conversation_id: 7, turn: 3 }))
        return json({ conversation_id: 7, turn: 3, pending: true }, 202)
      }
      return json([row(1, 'user', 'earlier'), row(2, 'assistant', 'old'), row(3, 'user', 'job'), row(4, 'assistant', 'new')])
    }),
  )
  const reply = talkTurn('job')
  await new Promise((r) => setTimeout(r, 20))
  release()
  expect((await reply).reply).toBe('new')
})
