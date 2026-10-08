'use strict'

const { contextBridge, ipcRenderer } = require('electron')

contextBridge.exposeInMainWorld('noteDesktop', {
  ring: () => ipcRenderer.send('desktop:ring'),
  show: () => ipcRenderer.send('desktop:show'),
  settings: () => ipcRenderer.invoke('desktop:settings'),
  set: (key, on) => ipcRenderer.invoke('desktop:set', key, on),
  changeServer: () => ipcRenderer.send('desktop:changeServer'),
})
