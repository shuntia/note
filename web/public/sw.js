self.addEventListener('install', () => self.skipWaiting())
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()))

self.addEventListener('push', (e) => {
  let data = { title: 'Note', body: '' }
  try {
    data = { ...data, ...e.data.json() }
  } catch {
    // non-JSON payload; show the defaults
  }
  e.waitUntil(
    self.registration.showNotification(data.title, {
      body: data.body,
      icon: '/icon.svg',
    }),
  )
})

self.addEventListener('notificationclick', (e) => {
  e.notification.close()
  e.waitUntil(
    self.clients.matchAll({ type: 'window' }).then((tabs) => {
      const tab = tabs.find((t) => 'focus' in t)
      return tab ? tab.focus() : self.clients.openWindow('/')
    }),
  )
})
