import { afterEach, expect, test, vi } from 'vitest'
import { api } from './api'

afterEach(() => vi.unstubAllGlobals())

test('the Matrix switches travel with a settings save', async () => {
  const fetch = vi.fn(async () => new Response('{}', { status: 200 }))
  vi.stubGlobal('fetch', fetch)
  await api.saveSettings({ matrix_send: true, matrix_ping: false })
  const [, init] = fetch.mock.calls[0] as unknown as [string, RequestInit]
  expect(JSON.parse(init.body as string)).toEqual({ matrix_send: true, matrix_ping: false })
})
