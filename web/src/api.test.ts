import { afterEach, expect, test, vi } from 'vitest'
import { api, ApiError } from './api'

afterEach(() => vi.unstubAllGlobals())

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
