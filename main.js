const { app, BrowserWindow, ipcMain } = require('electron');
const path = require('node:path');
const { checkCortexAccess, queryHeadsets } = require('./cortex');

let envLoadError;
try {
  process.loadEnvFile(path.join(__dirname, '.env'));
} catch (error) {
  envLoadError = error;
  console.error('Failed to load Cortex credentials from .env:', error);
}

ipcMain.handle('cortex:check-access', async () => {
  if (envLoadError) {
    throw new Error('Nem sikerült betölteni a .env fájlt. Ellenőrizd, hogy a projekt gyökerében létezik-e.');
  }

  const { client_id: clientId, client_secret: clientSecret } = process.env;
  if (!clientId || !clientSecret) {
    throw new Error('A .env fájlból hiányzik a client_id vagy a client_secret.');
  }

  return checkCortexAccess({ clientId, clientSecret });
});

ipcMain.handle('cortex:query-headsets', () => queryHeadsets());

const createWindow = () => {
  const win = new BrowserWindow({
    fullscreen: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js')
    }
  })

  win.loadFile('index.html')
}

app.whenReady().then(() => {
  createWindow()

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createWindow()
    }
  })
})

app.on('window-all-closed', () => {
  app.quit()
})