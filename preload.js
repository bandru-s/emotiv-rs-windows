const { contextBridge, ipcRenderer } = require('electron')

contextBridge.exposeInMainWorld('cortex', {
  checkAccess: () => ipcRenderer.invoke('cortex:check-access'),
  queryHeadsets: () => ipcRenderer.invoke('cortex:query-headsets')
})