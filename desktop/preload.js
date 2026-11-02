'use strict'

const { contextBridge, ipcRenderer } = require('electron')

function listen(channel, fn, read = (value) => value) {
  const listener = (_event, value) => fn(read(value))
  ipcRenderer.on(channel, listener)
  return () => ipcRenderer.removeListener(channel, listener)
}

contextBridge.exposeInMainWorld('noteDesktop', {
  ring: (info) => ipcRenderer.send('desktop:ring', info),
  ringStop: () => ipcRenderer.send('desktop:ringStop'),
  onRingAction: (fn) => listen('desktop:ringAction', fn, (action) => (action === 'answer' ? 'answer' : 'decline')),
  show: () => ipcRenderer.send('desktop:show'),
  settings: () => ipcRenderer.invoke('desktop:settings'),
  set: (key, on) => ipcRenderer.invoke('desktop:set', key, on),
  changeServer: () => ipcRenderer.send('desktop:changeServer'),
  inCall: (on) => ipcRenderer.send('desktop:inCall', on),
  widget: () => ipcRenderer.send('desktop:widget'),
  widgetClock: (clock) => ipcRenderer.send('desktop:widgetClock', clock),
  shown: () => ipcRenderer.invoke('desktop:shown'),
  onShown: (fn) => listen('desktop:shown', fn, (shown) => shown === true),
  micAccess: () => ipcRenderer.invoke('desktop:micAccess'),
  micSettings: () => ipcRenderer.send('desktop:micSettings'),
  onOpen: (fn) => {
    const stop = listen('desktop:open', fn, String)
    ipcRenderer.send('desktop:openReady')
    return stop
  },
  onResume: (fn) => listen('desktop:resume', () => fn()),
  about: () => ipcRenderer.invoke('desktop:about'),
})
