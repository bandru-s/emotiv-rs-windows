const { app, BrowserWindow, ipcMain } = require('electron');
const path = require('node:path');
const { spawn } = require('node:child_process');

let win;
let bridge;
let isQuitting = false;
let bridgeStatus = { type: 'status', state: 'starting', message: 'A Bluetooth-híd indítása...' };

function sendBridgeEvent(event) {
  if (event.type === 'status' || event.type === 'error') {
    bridgeStatus = event;
  }
  if (win && !win.isDestroyed()) {
    win.webContents.send('emotiv:event', event);
  }
}

function startBridge() {
  const executable = process.platform === 'win32' ? 'emotiv_bridge.exe' : 'emotiv_bridge';
  const bridgePath = path.join(__dirname, 'bridge', 'target', 'release', executable);
  bridge = spawn(bridgePath, [], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });

  let output = '';
  bridge.stdout.setEncoding('utf8');
  bridge.stdout.on('data', (chunk) => {
    output += chunk;
    const lines = output.split(/\r?\n/);
    output = lines.pop();
    for (const line of lines) {
      if (!line.trim()) continue;
      try {
        sendBridgeEvent(JSON.parse(line));
      } catch (error) {
        console.error('Invalid message from emotiv-rs bridge:', error);
        sendBridgeEvent({ type: 'error', message: 'Érvénytelen adat érkezett a Bluetooth-hídtól.' });
      }
    }
  });
  bridge.stderr.setEncoding('utf8');
  let diagnostics = '';
  bridge.stderr.on('data', (chunk) => {
    diagnostics += chunk;
    const lines = diagnostics.split(/\r?\n/);
    diagnostics = lines.pop();
    for (const line of lines) {
      if (!line.trim()) continue;
      console.error('emotiv-rs bridge:', line);
      if (/\b(?:WARN|ERROR)\b/.test(line)) {
        sendBridgeEvent({ type: 'diagnostic', message: line.trim() });
      }
    }
  });
  bridge.on('error', (error) => {
    console.error('Failed to start emotiv-rs bridge:', error);
    sendBridgeEvent({
      type: 'error',
      message: `Nem indult el a Rust Bluetooth-híd: ${error.message}. Futtasd az npm start parancsot a Rust buildeléséhez.`
    });
  });
  bridge.on('close', (code) => {
    bridge = null;
    if (app.isReady() && !isQuitting) {
      sendBridgeEvent({
        type: 'error',
        message: `A Bluetooth-híd leállt (kilépési kód: ${code ?? 'ismeretlen'}). Indítsd újra az alkalmazást.`
      });
    }
  });
}

const createWindow = () => {
  win = new BrowserWindow({
    fullscreen: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js')
    }
  });

  win.loadFile('index.html');
  win.webContents.on('did-finish-load', () => {
    win.webContents.send('emotiv:event', bridgeStatus);
  });
};

ipcMain.handle('emotiv:get-status', () => bridgeStatus);
ipcMain.handle('emotiv:command', (_event, command) => {
  if (!bridge || !bridge.stdin.writable) {
    throw new Error('A Bluetooth-híd nem fut. Indítsd újra az alkalmazást.');
  }
  if (!command || !['discover', 'connect', 'disconnect'].includes(command.command)) {
    throw new Error('Ismeretlen Bluetooth-parancs.');
  }
  if (command.command === 'connect' && typeof command.device !== 'string') {
    throw new Error('Válassz ki egy headsetet a csatlakozáshoz.');
  }
  bridge.stdin.write(`${JSON.stringify(command)}\n`);
});

app.whenReady().then(() => {
  createWindow();
  startBridge();

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createWindow();
    }
  });
});

app.on('window-all-closed', () => {
  app.quit();
});

app.on('before-quit', () => {
  isQuitting = true;
  if (bridge) {
    bridge.kill();
  }
});