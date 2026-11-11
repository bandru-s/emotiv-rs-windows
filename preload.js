const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('emotiv', {
  getStatus: () => ipcRenderer.invoke('emotiv:get-status'),
  sendCommand: (command) => ipcRenderer.invoke('emotiv:command', command),
  onEvent: (callback) => {
    const listener = (_event, data) => callback(data);
    ipcRenderer.on('emotiv:event', listener);
    return () => ipcRenderer.removeListener('emotiv:event', listener);
  }
});