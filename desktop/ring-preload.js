'use strict'

const { contextBridge, ipcRenderer } = require('electron')

contextBridge.exposeInMainWorld('noteRing', {
  onInfo: (fn) => ipcRenderer.on('ring:info', (_event, info) => fn(info)),
  answer: () => ipcRenderer.send('ring:answer'),
  decline: () => ipcRenderer.send('ring:decline'),
})
