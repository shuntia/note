import { afterEach, expect, test, vi } from 'vitest'
import { api, ApiError } from './api'

afterEach(() => vi.unstubAllGlobals())

test('a memory edit patches the fact and forgetting deletes it', async () => {
  const fetch = vi.fn(async () => new Response(null, { status: 204 }))
  vi.stubGlobal('fetch', fetch)
  await api.memoryEdit('a-b', { summary: 'likes oolong', body: '' })
  await api.memoryForget('a-b')
  const calls = fetch.mock.calls as unknown as [string, RequestInit][]
  expect(calls.map(([path, init]) => [path, init.method])).toEqual([
    ['/api/memory/a-b', 'PATCH'],
    ['/api/memory/a-b', 'DELETE'],
  ])
  expect(JSON.parse(calls[0][1].body as string)).toEqual({ summary: 'likes oolong', body: '' })
})

test('the mic list is read and a client is set by its encoded name', async () => {
  const row = { client: 'web:a/b', kind: 'web', label: 'USB', gain_db: 9, auto: false }
  const fetch = vi.fn(async () => new Response(JSON.stringify({ mics: [row] }), { status: 200 }))
  vi.stubGlobal('fetch', fetch)
  expect(await api.voiceMics()).toEqual({ mics: [row] })
  await api.voiceMicSet('web:a/b', { gain_db: 9 })
  await api.voiceMicSet('element', { auto: true })
  const calls = fetch.mock.calls as unknown as [string, RequestInit | undefined][]
  expect(calls.map(([path, init]) => [path, init?.method ?? 'GET'])).toEqual([
    ['/api/voice/mics', 'GET'],
    ['/api/voice/mics/web%3Aa%2Fb', 'PUT'],
    ['/api/voice/mics/element', 'PUT'],
  ])
  expect(JSON.parse(calls[1][1]?.body as string)).toEqual({ gain_db: 9 })
  expect(JSON.parse(calls[2][1]?.body as string)).toEqual({ auto: true })
})

test('the Matrix switches travel with a settings save', async () => {
  const fetch = vi.fn(async () => new Response('{}', { status: 200 }))
  vi.stubGlobal('fetch', fetch)
  await api.saveSettings({ matrix_send: true, matrix_ping: false })
  const [, init] = fetch.mock.calls[0] as unknown as [string, RequestInit]
  expect(JSON.parse(init.body as string)).toEqual({ matrix_send: true, matrix_ping: false })
})

test('a failed turn carries its reason and, for an admin, the detail', async () => {
  const body = { error: 'AIモデルが見つかりません。', reason: 'model_unavailable', detail: 'status 404: No endpoints found' }
  vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify(body), { status: 502 })))
  const err: unknown = await api.talk('hello').catch((e: unknown) => e)
  expect(err).toBeInstanceOf(ApiError)
  expect(err).toMatchObject({ status: 502, message: body.error, reason: 'model_unavailable', detail: body.detail })
})
