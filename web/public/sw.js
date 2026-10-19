self.addEventListener('install', () => self.skipWaiting())
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()))

self.addEventListener('push', (e) => {
  let data = { title: 'Note', body: '', url: '', actions: [] }
  try {
    data = { ...data, ...e.data.json() }
  } catch {
    // non-JSON payload; show the defaults
  }
  e.waitUntil(
    self.registration.showNotification(data.title, {
      body: data.body,
      icon: '/icon.svg',
      actions: data.actions,
      data: { url: data.url },
    }),
  )
})

// A button carries its own key, so pressing it needs no session; a press that
// fails opens the app instead, where the same choice can be made.
function press(key, url) {
  return fetch('/api/actions', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ key }),
  })
    .then((res) => {
      if (!res.ok && res.status !== 409) return land(url)
    })
    .catch(() => land(url))
}

// A notification that names a thread lands on it: an open tab is told where to
// go, and a fresh one opens there.
function land(url) {
  return self.clients.matchAll({ type: 'window' }).then((tabs) => {
    const tab = tabs.find((t) => 'focus' in t)
    if (!tab) return self.clients.openWindow(url)
    tab.postMessage({ type: 'open', url })
    return tab.focus()
  })
}

self.addEventListener('notificationclick', (e) => {
  e.notification.close()
  const url = (e.notification.data && e.notification.data.url) || '/'
  e.waitUntil(e.action ? press(e.action, url) : land(url))
})
