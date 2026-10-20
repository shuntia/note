'use strict'

const { contextBridge, ipcRenderer } = require('electron')

contextBridge.exposeInMainWorld('noteWidget', {
  onClock: (fn) => ipcRenderer.on('widget:clock', (_event, clock) => fn(clock)),
  close: () => ipcRenderer.send('widget:close'),
  show: () => ipcRenderer.send('widget:show'),
})
