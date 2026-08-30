import { api } from './api'

function applicationServerKey(base64url: string): Uint8Array<ArrayBuffer> {
  const padding = '='.repeat((4 - (base64url.length % 4)) % 4)
  const b64 = (base64url + padding).replace(/-/g, '+').replace(/_/g, '/')
  const raw = atob(b64)
  return Uint8Array.from(raw, (c) => c.charCodeAt(0))
}

export async function pushState(): Promise<'unsupported' | 'off' | 'on'> {
  if (!('serviceWorker' in navigator) || !('PushManager' in window)) return 'unsupported'
  const reg = await navigator.serviceWorker.ready
  const sub = await reg.pushManager.getSubscription()
  return sub ? 'on' : 'off'
}

export async function enablePush(): Promise<void> {
  const { key } = await api.vapidKey()
  const reg = await navigator.serviceWorker.ready
  const sub = await reg.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: applicationServerKey(key),
  })
  await api.pushSubscribe(sub.toJSON())
}

export async function disablePush(): Promise<void> {
  const reg = await navigator.serviceWorker.ready
  const sub = await reg.pushManager.getSubscription()
  if (!sub) return
  const { endpoint } = sub
  await sub.unsubscribe()
  await api.pushUnsubscribe(endpoint)
}
