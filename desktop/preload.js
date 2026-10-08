'use strict'

const { contextBridge, ipcRenderer } = require('electron')

contextBridge.exposeInMainWorld('noteDesktop', {
  ring: () => ipcRenderer.send('desktop:ring'),
})
