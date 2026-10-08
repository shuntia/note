'use strict'

const { contextBridge, ipcRenderer } = require('electron')

contextBridge.exposeInMainWorld('noteSetup', {
  connect: (address) => ipcRenderer.invoke('setup:connect', address),
})
