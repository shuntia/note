self.addEventListener('install', () => self.skipWaiting())
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()))

self.addEventListener('push', (e) => {
  let data = { title: 'Note', body: '', url: '' }
  try {
    data = { ...data, ...e.data.json() }
  } catch {
    // non-JSON payload; show the defaults
  }
  e.waitUntil(
    self.registration.showNotification(data.title, {
      body: data.body,
      icon: '/icon.svg',
      data: { url: data.url },
    }),
  )
})

// A notification that names a thread lands on it: an open tab is told where to
// go, and a fresh one opens there.
self.addEventListener('notificationclick', (e) => {
  e.notification.close()
  const url = (e.notification.data && e.notification.data.url) || '/'
  e.waitUntil(
    self.clients.matchAll({ type: 'window' }).then((tabs) => {
      const tab = tabs.find((t) => 'focus' in t)
      if (!tab) return self.clients.openWindow(url)
      tab.postMessage({ type: 'open', url })
      return tab.focus()
    }),
  )
})
