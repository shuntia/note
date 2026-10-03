import { api, ApiError } from './api'

const READY_TIMEOUT_MS = 5000

function applicationServerKey(base64url: string): Uint8Array<ArrayBuffer> {
  const padding = '='.repeat((4 - (base64url.length % 4)) % 4)
  const b64 = (base64url + padding).replace(/-/g, '+').replace(/_/g, '/')
  const raw = atob(b64)
  return Uint8Array.from(raw, (c) => c.charCodeAt(0))
}

// `serviceWorker.ready` never rejects, so a registration that never activates
// would hang every caller.
function serviceWorkerReady(): Promise<ServiceWorkerRegistration> {
  const timeout = new Promise<never>((_, reject) => {
    const fail = () => reject(new Error('service worker never became ready'))
    window.setTimeout(fail, READY_TIMEOUT_MS)
  })
  return Promise.race([navigator.serviceWorker.ready, timeout])
}

// Electron exposes PushManager but has no push service behind it.
export async function pushState(): Promise<'unsupported' | 'off' | 'on'> {
  if (!('serviceWorker' in navigator) || !('PushManager' in window) || navigator.userAgent.includes('Electron/')) return 'unsupported'
  const reg = await serviceWorkerReady()
  const sub = await reg.pushManager.getSubscription()
  return sub ? 'on' : 'off'
}

export async function enablePush(): Promise<void> {
  const { key } = await api.vapidKey()
  const reg = await serviceWorkerReady()
  const sub = await reg.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: applicationServerKey(key),
  })
  try {
    await api.pushSubscribe(sub.toJSON())
  } catch (err) {
    // an orphaned browser subscription would read as "on" next visit
    await sub.unsubscribe()
    throw err
  }
}

export async function disablePush(): Promise<void> {
  const reg = await serviceWorkerReady()
  const sub = await reg.pushManager.getSubscription()
  if (!sub) return
  try {
    await api.pushUnsubscribe(sub.endpoint)
  } catch (err) {
    if (!(err instanceof ApiError) || err.status !== 404) throw err
  }
  await sub.unsubscribe()
}
