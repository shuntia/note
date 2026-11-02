'use strict'

const { contextBridge, ipcRenderer } = require('electron')

contextBridge.exposeInMainWorld('noteLauncher', {
  hide: () => ipcRenderer.send('launcher:hide'),
  resize: (height) => ipcRenderer.send('launcher:resize', Number(height)),
  open: (conversationId) => ipcRenderer.send('launcher:open', conversationId),
  onShown: (fn) => {
    const listener = () => fn()
    ipcRenderer.on('launcher:shown', listener)
    return () => ipcRenderer.removeListener('launcher:shown', listener)
  },
})
