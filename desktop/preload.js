'use strict'

const { contextBridge, ipcRenderer } = require('electron')

contextBridge.exposeInMainWorld('noteDesktop', {
  ring: () => ipcRenderer.send('desktop:ring'),
  show: () => ipcRenderer.send('desktop:show'),
  settings: () => ipcRenderer.invoke('desktop:settings'),
  set: (key, on) => ipcRenderer.invoke('desktop:set', key, on),
  changeServer: () => ipcRenderer.send('desktop:changeServer'),
  widget: () => ipcRenderer.send('desktop:widget'),
  widgetClock: (clock) => ipcRenderer.send('desktop:widgetClock', clock),
  shown: () => ipcRenderer.invoke('desktop:shown'),
  onShown: (fn) => {
    const listener = (_event, shown) => fn(shown === true)
    ipcRenderer.on('desktop:shown', listener)
    return () => ipcRenderer.removeListener('desktop:shown', listener)
  },
})
